//! Intent classifier — does this message want an answer or work?
//!
//! The chat TUI dispatches every message under a lead role. The default
//! is `fixer`, which is wrapped in the coding workflow (run tests,
//! iterate until green, write files freely). For a question like "how
//! do I run dev mode" that's the wrong shape: the agent ends up writing
//! `DEV_MODE_GUIDE.md` files and inventing a "1 passed, 0 failed" line
//! to satisfy the workflow's report rule.
//!
//! Routing strategy (pearl th-c677f7 — Chief of Staff):
//!
//! 1. **Primary**: [`classify_via_chief`] calls the `chief` shadow role
//!    (Fast slot — Haiku-class) with the user message. Chief picks one
//!    of the lead/sidekick roles and emits `DISPATCH: <role>`. The full
//!    cast (fixer, oracle, scout, mapper, recapper) is available, not
//!    just Work/Question.
//!
//! 2. **Fallback**: when the chief LLM is unavailable (no providers,
//!    gateway down, unparseable response), the legacy heuristic ladder
//!    runs ([`looks_like_shell_op`] + [`looks_like_vague_improve`] +
//!    [`looks_like_factual_shell_query`] + [`classify_via_llm`] against
//!    the older `intent_classifier` shadow role). Dispatch never hangs.
//!
//! The classifier only runs when [`crate::state::AppState::agent_pinned`]
//! is `false`. `/ask`, `/agent <name>`, `--agent <name>`, and resuming
//! a saved session all pin the role and disable auto-routing.

/// What role should handle this turn. Extended from the original
/// Work/Question binary to cover every lead and sidekick the chief
/// can dispatch to (pearl th-c677f7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Read-only question. Routes to `oracle`.
    Question,
    /// Wants the agent to do work (write/edit code, run tests, etc.).
    /// Routes to `fixer` + coding workflow.
    Work,
    /// Exploratory investigation — routes to the `scout` sidekick.
    Scout,
    /// Symbol-level navigation — routes to the `mapper` sidekick.
    Mapper,
    /// Recap / status summary — routes to the `recapper` shadow.
    Recap,
}

impl Intent {
    /// Return the role this intent should dispatch under.
    pub fn role(self) -> &'static str {
        match self {
            Self::Question => "oracle",
            Self::Work => "fixer",
            Self::Scout => "scout",
            Self::Mapper => "mapper",
            Self::Recap => "recapper",
        }
    }

    /// Parse a role name (as emitted by the chief LLM) into an
    /// `Intent`. Returns `None` for unknown role names.
    #[must_use]
    pub fn from_role_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "fixer" | "fix" | "coder" | "work" => Some(Self::Work),
            "oracle" | "question" | "qa" | "ask" => Some(Self::Question),
            "scout" => Some(Self::Scout),
            "mapper" | "map" => Some(Self::Mapper),
            "recapper" | "recap" | "summary" | "summarizer" => Some(Self::Recap),
            _ => None,
        }
    }
}

/// Classify a single user message via the `intent_classifier` shadow
/// role. Falls back to [`classify_heuristic`] when the LLM call fails
/// (no providers, gateway unreachable, unparseable response) so a
/// transient outage doesn't strand the chat.
///
/// Special case: if the message reads as a git/shell-op request
/// ([`looks_like_shell_op`]) we override to `Question` regardless of
/// LLM/heuristic verdict. The sandboxed runner has no `git_commit` /
/// `bash_host` tool, so dispatching to fixer's coding workflow on a
/// "commit this" request hallucinates a fake fix loop. Routing to
/// oracle at least produces a "I can't run git from the sandbox;
/// here's the command" answer instead of a hallucinated diff
/// (pearl th-919f1e).
pub async fn classify(message: &str) -> Intent {
    if message.trim().is_empty() {
        return Intent::Work;
    }

    // Primary path: ask the Chief of Staff (pearl th-c677f7).
    // Chief sees the full message and picks one of the lead/sidekick
    // roles — single LLM call replaces the heuristic ladder.
    if let Some(intent) = classify_via_chief(message).await {
        return intent;
    }

    // Fallback ladder kicks in when chief is unavailable (no providers,
    // gateway down, unparseable response). The heuristics catch the
    // shapes we've observed cause concrete failures in the field:

    if looks_like_shell_op(message) {
        // Pearl th-bench-loop iter 23: "what's the git status" routed
        // to oracle (Question) because it contains "git", but oracle
        // can't run bash — it ends up grepping .git/HEAD and inferring
        // badly. Factual shell questions need a shell. Route those to
        // Work instead.
        if looks_like_factual_shell_query(message) {
            return Intent::Work;
        }
        return Intent::Question;
    }
    if looks_like_vague_improve(message) {
        // Pearl iter-8: "make X better" / "clean up Y" / "polish Z"
        // sent to fixer triggers wide rewrites the user didn't ask
        // for. Route to oracle instead — it'll respond with a
        // clarifying question.
        return Intent::Question;
    }
    match classify_via_llm(message).await {
        Some(intent) => intent,
        None => classify_heuristic(message),
    }
}

/// Call the `chief` shadow role and parse its `DISPATCH: <role>`
/// response into an [`Intent`]. Returns `None` when the LLM is
/// unavailable or its output can't be parsed — caller falls through
/// to the heuristic ladder.
async fn classify_via_chief(message: &str) -> Option<Intent> {
    use smooth_operator::cast::Cast;
    use smooth_operator::providers::ProviderRegistry;

    let providers_path = dirs_next::home_dir()?.join(".smooth/providers.json");
    let registry = ProviderRegistry::load_from_file(&providers_path).ok()?;
    let cast = Cast::builtin();
    let role = cast.get("chief")?;
    let config = registry.llm_config_for(role.slot).ok()?;
    let llm = smooth_operator::llm::LlmClient::new(config);

    let system = smooth_operator::conversation::Message::system(&role.prompt);
    let user = smooth_operator::conversation::Message::user(message);
    let resp = llm.chat(&[&system, &user], &[]).await.ok()?;

    parse_chief_response(&resp.content)
}

/// Parse a chief response into an [`Intent`]. The prompt asks for
/// `DISPATCH: <role>`; we look for that exact shape but also fall
/// back to scanning for a bare role name when the model misformats
/// (some Haiku-class models omit the prefix).
#[must_use]
fn parse_chief_response(content: &str) -> Option<Intent> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Primary shape: "DISPATCH: <role>". Anchor on the colon so we
    // pick up the token even when the model adds a leading word like
    // "Final answer: DISPATCH: fixer" or trailing punctuation.
    if let Some(after) = trimmed.to_ascii_uppercase().find("DISPATCH:") {
        let tail = &trimmed[after + "DISPATCH:".len()..];
        let role = tail.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric());
        if let Some(intent) = Intent::from_role_name(role) {
            return Some(intent);
        }
    }
    // Fallback: search the response for any of the role names in
    // priority order — fixer wins ties (same conservative bias as
    // the legacy classifier).
    let upper = trimmed.to_ascii_uppercase();
    for role in ["FIXER", "SCOUT", "MAPPER", "RECAPPER", "ORACLE"] {
        if upper.contains(role) {
            return Intent::from_role_name(role);
        }
    }
    None
}

/// Heuristic for "this is a vague self-improvement ask, not a
/// concrete coding task." Matches asks like "make X better",
/// "clean up the code", "improve this", "polish the README",
/// "tidy up the imports" — where there's a fuzzy adjective and
/// no concrete change named.
///
/// We're deliberately narrow on what counts as vague: a fuzzy
/// adjective alone, OR with "the X" / "this" / "it". A phrase
/// like "improve performance of the parser" is concrete (says
/// what to improve and where) and stays as Work.
///
/// Public for tests only.
#[must_use]
pub fn looks_like_vague_improve(message: &str) -> bool {
    let lower = message.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    // Trigger phrases — vague verbs/adjectives that on their own
    // (or with a generic object) mean "agent please figure out
    // what to do." Each phrase must appear surrounded by word
    // boundaries to avoid false matches like "improvement test"
    // or "polished glass icon".
    const VAGUE_PHRASES: &[&str] = &[
        // "make it better" family — pronoun + fuzzy adjective.
        "make it better",
        "make it nicer",
        "make it cleaner",
        "make this better",
        "make this nicer",
        "make this cleaner",
        // Verb + pronoun forms — explicit fuzz on a referent.
        "clean it up",
        "polish this",
        "polish it",
        "modernize this",
        "modernize it",
        // "tidy up" alone — almost always vague.
        "tidy up",
    ];
    for phrase in VAGUE_PHRASES {
        if lower.contains(phrase) {
            return true;
        }
    }
    // "make X better" / "make X cleaner" / "improve X" patterns
    // where X is short (a single token like a filename or
    // identifier) and there's no further qualifier. The "make X
    // better" pattern caught us in iter 7-8.
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if let Some(idx) = tokens.iter().position(|t| *t == "make") {
        // Need at least "make X <fuzzy_adj>" — 3 tokens after "make"
        // makes the pattern a 4-token-or-shorter ask.
        let rest = &tokens[idx + 1..];
        if rest.len() <= 3 {
            // Last word is one of the fuzzy adjectives?
            if let Some(last) = rest.last() {
                let stripped = last.trim_end_matches(|c: char| !c.is_alphanumeric());
                if matches!(stripped, "better" | "nicer" | "cleaner" | "prettier" | "more" | "best") {
                    return true;
                }
            }
        }
    }
    // "improve X" / "polish X" / "clean up X" with a SHORT object
    // (no specific quality/dimension named) → vague.
    // "improve the parser performance" stays as Work; "improve
    // this" / "improve App.tsx" goes Question.
    for verb in ["improve", "polish", "modernize"] {
        if let Some(rest) = lower.split_whitespace().skip_while(|t| *t != verb).nth(1) {
            // After the verb there's only one token (the object) —
            // no quality or dimension named.
            let after_count = lower.split_whitespace().skip_while(|t| *t != verb).count();
            // skip_while gives us [verb, object, ...] so count >= 1.
            // count == 2 means [verb, object] only.
            if after_count == 2 && !rest.is_empty() {
                return true;
            }
        }
    }
    false
}

/// Heuristic for "this is a git/shell operation request, not a
/// coding task." Matches messages whose primary verb is a git or
/// shell command. The runner's tool surface is filesystem +
/// project-inspect + bash *inside* the sandbox; it cannot push a
/// commit to a host remote, can't `gh pr create`, etc. Better to
/// surface the right command than to dispatch a sandboxed coding
/// agent that will hallucinate "I committed it!"
///
/// Public for tests only.
#[must_use]
pub fn looks_like_shell_op(message: &str) -> bool {
    let lower = message.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    // Tokenise on whitespace + common punctuation so "can we commit"
    // and "commit," and "commit this" all surface "commit" as a token.
    // Hyphens stay inside tokens so "cherry-pick" is one token, not
    // two, and matches the SHELL_OP_VERBS entry.
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.is_empty() {
        return false;
    }
    // Direct match: any of the shell-op verbs appears as a standalone
    // token. Captures "can we commit", "let's push", "merge to main",
    // "git add this", "rebase onto", etc.
    for tok in &tokens {
        if SHELL_OP_VERBS.contains(tok) {
            return true;
        }
    }
    false
}

/// Verbs / keywords that indicate a git-or-shell operation the
/// sandboxed runner can't execute. Kept narrow on purpose: words
/// that are also common in coding-task asks (run, test, build,
/// install, branch, merge of types) stay in WORK_VERBS so "run the
/// test suite" / "merge these two functions" still dispatches to
/// fixer.
///
/// "git" and "gh" are the unambiguous wins — they almost always
/// mean "execute a git/gh command for me." The verbs commit / push
/// / rebase / amend are git-flavored enough that even when used as
/// English ("can we commit to this approach") the user benefits
/// from oracle's read-only response over fixer's hallucinated
/// "I committed your code!"
const SHELL_OP_VERBS: &[&str] = &["git", "gh", "commit", "push", "rebase", "amend", "stash", "cherry-pick", "checkout"];

/// True when a shell-op-flavored message reads as a FACTUAL request
/// ("what's the git status", "show me the diff", "list staged
/// files") rather than a policy/advice request ("should I rebase",
/// "can we commit"). Factual asks need real shell execution and
/// belong in fixer's tool registry, not oracle's read-only loop.
///
/// Pearl th-bench-loop iter 23 / user transcript 2026-05-10:
/// oracle handling "what's the git status" couldn't run `git
/// status` (bash blocked by PermissionHook on oracle), so it
/// reverse-engineered the answer from .git/HEAD + .git/objects
/// reads and got it mostly wrong. Routing to fixer gives the
/// agent real shell access and a one-line authoritative answer.
///
/// Public for tests only.
#[must_use]
pub fn looks_like_factual_shell_query(message: &str) -> bool {
    let lower = message.trim().to_ascii_lowercase();
    // Question-shaped openers that imply "go look and tell me".
    const FACTUAL_OPENERS: &[&str] = &[
        "what's",
        "whats",
        "what is",
        "what are",
        "what's the",
        "show me",
        "show the",
        "show all",
        "list the",
        "list all",
        "list every",
        "tell me what",
        "tell me which",
        "tell me the",
        "how many",
        "which files",
        "which file",
        "which branch",
        "are there",
        "is there",
        "do we have",
        "any uncommitted",
        "any unstaged",
        "any staged",
        "any unpushed",
    ];
    // Counter-examples FIRST: "should we…" / "is it safe to…" /
    // "what's the best way…" are policy questions and stay with
    // oracle, even when they pattern-match a factual opener like
    // "what's…". Check policy first to short-circuit before the
    // factual-opener match.
    const POLICY_MARKERS: &[&str] = &[
        "should i",
        "should we",
        "should you",
        "is it safe",
        "is it ok",
        "would it be",
        "can i safely",
        "can we safely",
        "would you recommend",
        "what's the best way",
        "best way to",
    ];
    for marker in POLICY_MARKERS {
        if lower.contains(marker) {
            return false;
        }
    }
    for opener in FACTUAL_OPENERS {
        if lower.starts_with(opener) {
            return true;
        }
    }
    // If the message contains "can you" + a shell verb, treat as
    // factual ("can you run git status", "can you show me the diff").
    if lower.starts_with("can you ") {
        return true;
    }
    false
}

async fn classify_via_llm(message: &str) -> Option<Intent> {
    use smooth_operator::cast::Cast;
    use smooth_operator::providers::ProviderRegistry;

    let providers_path = dirs_next::home_dir()?.join(".smooth/providers.json");
    let registry = ProviderRegistry::load_from_file(&providers_path).ok()?;
    let cast = Cast::builtin();
    let role = cast.get("intent_classifier")?;
    let config = registry.llm_config_for(role.slot).ok()?;
    let llm = smooth_operator::llm::LlmClient::new(config);

    let system = smooth_operator::conversation::Message::system(&role.prompt);
    let user = smooth_operator::conversation::Message::user(message);
    let resp = llm.chat(&[&system, &user], &[]).await.ok()?;

    parse_llm_response(&resp.content)
}

fn parse_llm_response(content: &str) -> Option<Intent> {
    // The prompt asks for a literal `WORK` or `QUESTION` token. Models
    // sometimes wrap it in punctuation or add filler ("Answer: WORK"),
    // so search the response uppercase rather than requiring an exact
    // match. WORK takes priority on ties — same conservative bias as
    // the heuristic and the prompt's ambiguity rule.
    let upper = content.to_ascii_uppercase();
    let has_work = upper.contains("WORK");
    let has_question = upper.contains("QUESTION");
    match (has_work, has_question) {
        (true, false) => Some(Intent::Work),
        (false, true) => Some(Intent::Question),
        (true, true) => Some(Intent::Work),
        (false, false) => None,
    }
}

/// Cheap pattern-matching fallback used when the LLM call can't run.
/// Public for tests; the dispatch path goes through [`classify`].
pub fn classify_heuristic(message: &str) -> Intent {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Intent::Work;
    }

    if trimmed.trim_end_matches(|c: char| c.is_whitespace() || c == '.').ends_with('?') {
        return Intent::Question;
    }

    let lower = trimmed.to_ascii_lowercase();
    for prefix in ["can you ", "could you ", "would you ", "will you ", "please "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            if rest.split_whitespace().next().is_some_and(|w| WORK_VERBS.contains(&w)) {
                return Intent::Work;
            }
            return Intent::Question;
        }
    }

    let first_word = trimmed
        .split(|c: char| c.is_whitespace() || c == ',')
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();

    if QUESTION_OPENERS.contains(&first_word.as_str()) {
        return Intent::Question;
    }
    if WORK_VERBS.contains(&first_word.as_str()) {
        return Intent::Work;
    }

    // Default to Work — preserves prior behavior on ambiguous input.
    Intent::Work
}

const QUESTION_OPENERS: &[&str] = &[
    "how",
    "what",
    "why",
    "when",
    "where",
    "who",
    "which",
    "whose",
    "is",
    "are",
    "was",
    "were",
    "am",
    "do",
    "does",
    "did",
    "can",
    "could",
    "should",
    "would",
    "will",
    "shall",
    "may",
    "might",
    "explain",
    "describe",
    "summarize",
    "summarise",
    "tell",
    "remind",
    "clarify",
    "compare",
];

const WORK_VERBS: &[&str] = &[
    "fix",
    "add",
    "implement",
    "refactor",
    "write",
    "create",
    "build",
    "make",
    "rename",
    "move",
    "delete",
    "remove",
    "patch",
    "edit",
    "change",
    "modify",
    "update",
    "upgrade",
    "downgrade",
    "install",
    "uninstall",
    "configure",
    "set",
    "wire",
    "plumb",
    "extract",
    "split",
    "merge",
    "rebase",
    "commit",
    "push",
    "deploy",
    "run",
    "test",
    "lint",
    "format",
    "generate",
    "regenerate",
    "migrate",
    "seed",
    "scaffold",
    "bump",
    "introduce",
    "convert",
    "port",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_work_token() {
        assert_eq!(parse_llm_response("WORK"), Some(Intent::Work));
        assert_eq!(parse_llm_response("Answer: WORK"), Some(Intent::Work));
        assert_eq!(parse_llm_response("  work  "), Some(Intent::Work));
    }

    #[test]
    fn parse_question_token() {
        assert_eq!(parse_llm_response("QUESTION"), Some(Intent::Question));
        assert_eq!(parse_llm_response("Question."), Some(Intent::Question));
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(parse_llm_response(""), None);
        assert_eq!(parse_llm_response("maybe"), None);
        assert_eq!(parse_llm_response("I think this is..."), None);
    }

    #[test]
    fn parse_both_tokens_prefers_work() {
        // Ambiguous LLM responses default to Work — same conservative
        // bias as classify_heuristic so the user never silently loses
        // the ability to act.
        assert_eq!(parse_llm_response("WORK or QUESTION"), Some(Intent::Work));
    }

    #[test]
    fn chief_parses_canonical_dispatch_line() {
        // Pearl th-c677f7: the chief role emits "DISPATCH: <role>"
        // verbatim per its prompt. The parser must handle the canonical
        // shape across every routeable role.
        for (resp, expected) in &[
            ("DISPATCH: fixer", Intent::Work),
            ("DISPATCH: oracle", Intent::Question),
            ("DISPATCH: scout", Intent::Scout),
            ("DISPATCH: mapper", Intent::Mapper),
            ("DISPATCH: recapper", Intent::Recap),
        ] {
            assert_eq!(parse_chief_response(resp), Some(*expected), "canonical: {resp:?}");
        }
    }

    #[test]
    fn chief_parses_role_aliases() {
        // Models sometimes shorten or paraphrase. The from_role_name
        // alias table covers the ones we've observed.
        assert_eq!(parse_chief_response("DISPATCH: fix"), Some(Intent::Work));
        assert_eq!(parse_chief_response("DISPATCH: ask"), Some(Intent::Question));
        assert_eq!(parse_chief_response("DISPATCH: recap"), Some(Intent::Recap));
        assert_eq!(parse_chief_response("DISPATCH: map"), Some(Intent::Mapper));
    }

    #[test]
    fn chief_handles_filler_around_dispatch() {
        // Wrapper text, trailing punctuation, leading prose — the
        // parser anchors on the literal `DISPATCH:` so it survives all
        // of these.
        assert_eq!(parse_chief_response("Final answer: DISPATCH: fixer."), Some(Intent::Work));
        assert_eq!(parse_chief_response("  DISPATCH: oracle\n"), Some(Intent::Question));
        assert_eq!(parse_chief_response("Routing decision...\nDISPATCH: scout"), Some(Intent::Scout));
    }

    #[test]
    fn chief_falls_back_to_role_search_when_prefix_missing() {
        // Some Haiku-class models drop the DISPATCH: prefix and emit
        // just the role name. The fallback finds it.
        assert_eq!(parse_chief_response("fixer"), Some(Intent::Work));
        assert_eq!(parse_chief_response("This is an oracle question"), Some(Intent::Question));
    }

    #[test]
    fn chief_unparseable_returns_none() {
        // Whitespace, empty, prose without any role token → None so
        // the caller falls through to the heuristic ladder.
        assert_eq!(parse_chief_response(""), None);
        assert_eq!(parse_chief_response("   "), None);
        assert_eq!(parse_chief_response("I'm not sure"), None);
    }

    #[test]
    fn intent_role_round_trip() {
        // Every Intent variant must round-trip through role() →
        // from_role_name(). Guards against adding a new Intent
        // variant without wiring its alias.
        for intent in [Intent::Question, Intent::Work, Intent::Scout, Intent::Mapper, Intent::Recap] {
            let name = intent.role();
            assert_eq!(Intent::from_role_name(name), Some(intent), "round-trip {intent:?} via {name}");
        }
    }

    #[test]
    fn factual_shell_queries_recognized() {
        // Pearl th-bench-loop iter 23: these all need real shell.
        for q in &[
            "what's the git status",
            "what is the current branch",
            "show me the diff",
            "list the staged files",
            "tell me what files changed",
            "any uncommitted changes?",
            "is there anything unpushed",
            "how many commits ahead of main are we",
            "can you run git status",
            "which files are modified",
        ] {
            assert!(looks_like_factual_shell_query(q), "must classify as factual shell query: {q:?}");
        }
    }

    #[test]
    fn policy_shell_questions_stay_oracle() {
        // Policy-shaped questions about shell ops should NOT become
        // factual queries — they really do belong in oracle.
        for q in &[
            "should I rebase or merge",
            "is it safe to force-push",
            "would you recommend rebasing this branch",
            "what's the best way to handle this merge conflict",
            "can we safely commit secrets here",
        ] {
            assert!(!looks_like_factual_shell_query(q), "policy question must NOT be a factual shell query: {q:?}");
        }
    }

    #[test]
    fn factual_shell_takes_precedence_over_shell_op() {
        // The shell-op verb triggers the original Question route,
        // BUT the factual-shell guard should flip it back to Work.
        // We can verify the predicate composition without async:
        assert!(looks_like_shell_op("what's the git status"));
        assert!(looks_like_factual_shell_query("what's the git status"));
        assert!(looks_like_shell_op("should I commit this"));
        assert!(!looks_like_factual_shell_query("should I commit this"));
    }

    #[test]
    fn heuristic_question_mark() {
        assert_eq!(classify_heuristic("how do I run dev mode?"), Intent::Question);
        assert_eq!(classify_heuristic("really?"), Intent::Question);
    }

    #[test]
    fn heuristic_interrogative_opener() {
        assert_eq!(classify_heuristic("how would you run the dev mode in this project"), Intent::Question);
        assert_eq!(classify_heuristic("what does this function do"), Intent::Question);
        assert_eq!(classify_heuristic("why is this failing"), Intent::Question);
    }

    #[test]
    fn heuristic_explain_prefix() {
        assert_eq!(classify_heuristic("explain how the orchestrator dispatches tasks"), Intent::Question);
        assert_eq!(classify_heuristic("describe the policy generation flow"), Intent::Question);
        assert_eq!(classify_heuristic("summarize what this PR changes"), Intent::Question);
    }

    #[test]
    fn heuristic_imperative_verb() {
        assert_eq!(classify_heuristic("fix the failing test in policy.rs"), Intent::Work);
        assert_eq!(classify_heuristic("add a test for the new endpoint"), Intent::Work);
        assert_eq!(classify_heuristic("refactor the dispatch path"), Intent::Work);
        assert_eq!(classify_heuristic("implement the worktree command"), Intent::Work);
    }

    #[test]
    fn heuristic_can_you_disambiguates() {
        assert_eq!(classify_heuristic("can you fix the build"), Intent::Work);
        assert_eq!(classify_heuristic("Can you implement that"), Intent::Work);
        assert_eq!(classify_heuristic("can you explain how this dispatches"), Intent::Question);
        assert_eq!(classify_heuristic("Can you tell me what changed"), Intent::Question);
    }

    #[test]
    fn heuristic_ambiguous_defaults_to_work() {
        assert_eq!(classify_heuristic("the test is broken"), Intent::Work);
        assert_eq!(classify_heuristic("hmm"), Intent::Work);
    }

    #[test]
    fn heuristic_empty_is_work() {
        assert_eq!(classify_heuristic(""), Intent::Work);
        assert_eq!(classify_heuristic("   "), Intent::Work);
    }

    #[test]
    fn role_mapping() {
        assert_eq!(Intent::Question.role(), "oracle");
        assert_eq!(Intent::Work.role(), "fixer");
    }

    #[test]
    fn shell_op_detection_catches_git_verbs() {
        // Pearl th-919f1e: messages that read as git/shell-op
        // requests should NOT route to fixer's coding workflow.
        // Verbatim from the bug report:
        assert!(looks_like_shell_op("can we commit that to main"));
        // Plus other common phrasings:
        assert!(looks_like_shell_op("commit and push"));
        assert!(looks_like_shell_op("git status"));
        assert!(looks_like_shell_op("let's push to origin"));
        assert!(looks_like_shell_op("rebase onto main"));
        assert!(looks_like_shell_op("amend the last commit"));
        assert!(looks_like_shell_op("checkout the feature branch"));
        assert!(looks_like_shell_op("gh pr create"));
        assert!(looks_like_shell_op("stash these changes"));
        assert!(looks_like_shell_op("cherry-pick that fix"));
    }

    #[test]
    fn vague_improve_detection_catches_iter8_bug() {
        // Pearl iter-8 verbatim: "make App.tsx better" triggered
        // fixer rewriting the file. Now routes to oracle so it
        // asks "what does better mean here" instead.
        assert!(looks_like_vague_improve("make App.tsx better"));
        assert!(looks_like_vague_improve("make this better"));
        assert!(looks_like_vague_improve("make it cleaner"));
        assert!(looks_like_vague_improve("Make this nicer"));
        // Pronoun + fuzzy verb forms.
        assert!(looks_like_vague_improve("clean it up"));
        assert!(looks_like_vague_improve("polish this"));
        assert!(looks_like_vague_improve("modernize it"));
        // "tidy up" alone — almost always vague.
        assert!(looks_like_vague_improve("tidy up"));
        // "improve X" / "polish X" / "modernize X" with no further
        // qualifier — caught by the 2-token verb-object pattern.
        assert!(looks_like_vague_improve("improve App.tsx"));
        assert!(looks_like_vague_improve("polish README"));
    }

    #[test]
    fn vague_improve_does_not_overmatch_concrete_asks() {
        // Concrete asks must stay as Work (route to fixer).
        // Anything with a specific dimension (performance, error
        // handling, a specific function) is concrete.
        assert!(!looks_like_vague_improve("improve performance of the parser"));
        assert!(!looks_like_vague_improve("improve the error handling in fetchUser"));
        assert!(!looks_like_vague_improve("polish the README's API examples"));
        // "make X <verb>" where the trailing word isn't a fuzzy adjective.
        assert!(!looks_like_vague_improve("make App.tsx render a list of items"));
        assert!(!looks_like_vague_improve("make App.tsx render properly"));
        // Concrete verbs unaffected.
        assert!(!looks_like_vague_improve("fix the failing test in policy.rs"));
        assert!(!looks_like_vague_improve("add a button to the header"));
    }

    #[test]
    fn shell_op_detection_does_not_overmatch_coding_verbs() {
        // Words that are common in coding asks but NOT shell ops.
        // These must continue to route normally (Work/fixer for verbs,
        // Question/oracle for questions).
        assert!(!looks_like_shell_op("merge these two functions"));
        assert!(!looks_like_shell_op("run the tests"));
        assert!(!looks_like_shell_op("install lodash"));
        assert!(!looks_like_shell_op("build the project"));
        assert!(!looks_like_shell_op("create a new branch in the parser"));
        assert!(!looks_like_shell_op("what does this do"));
        assert!(!looks_like_shell_op("fix the failing test"));
    }
}
