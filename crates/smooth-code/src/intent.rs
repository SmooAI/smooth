//! Intent classifier — does this message want an answer or work?
//!
//! The chat TUI dispatches every message under a lead role. The default
//! is `fixer`, which is wrapped in the coding workflow (run tests,
//! iterate until green, write files freely). For a question like "how
//! do I run dev mode" that's the wrong shape: the agent ends up writing
//! `DEV_MODE_GUIDE.md` files and inventing a "1 passed, 0 failed" line
//! to satisfy the workflow's report rule.
//!
//! [`classify`] sends the user message to the `intent_classifier`
//! shadow role (read-only, Fast slot — Haiku-class) and parses its
//! response as `WORK` or `QUESTION`. If the LLM is unavailable or its
//! response can't be parsed, [`classify_heuristic`] runs as a fallback
//! so dispatch never hangs on a flaky gateway.
//!
//! The classifier only runs when [`crate::state::AppState::agent_pinned`]
//! is `false`. `/ask`, `/agent <name>`, `--agent <name>`, and resuming
//! a saved session all pin the role and disable auto-routing.

/// What the user appears to want from this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Read-only — wants information, not changes. Routes to `oracle`.
    Question,
    /// Wants the agent to do work (write/edit code, run tests, etc.).
    /// Routes to `fixer` + coding workflow.
    Work,
}

impl Intent {
    /// Return the lead role this intent should dispatch under.
    pub fn role(self) -> &'static str {
        match self {
            Self::Question => "oracle",
            Self::Work => "fixer",
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
    if looks_like_shell_op(message) {
        return Intent::Question;
    }
    match classify_via_llm(message).await {
        Some(intent) => intent,
        None => classify_heuristic(message),
    }
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
