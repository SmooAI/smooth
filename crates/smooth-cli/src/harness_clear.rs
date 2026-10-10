//! `/th-clear` — budgeted handoff → the harness's own clear → automatic
//! resume (SMOODEV-3759, pearl th-957d9a).
//!
//! No harness lets a plugin clear its own context, so `/th-clear` is a
//! two-step dance the plugin choreographs:
//!
//! 1. `th harness handoff arm --pearl <id>` — after the th-handoff checkpoint,
//!    drop a one-shot token in `~/.smooth/handoff/armed/`, keyed on the cwd and
//!    the harness process that owns this session (found by walking the process
//!    tree, so it survives `/clear` and tells two terminals in one checkout
//!    apart). It expires after `--ttl-minutes`.
//! 2. The user types the harness's clear (`/clear`). Its SessionStart hook runs
//!    `th harness handoff claim`, which takes the token (atomic rename — one
//!    claimer wins), re-stamps each pearl with the NEW session id and prints
//!    the handoff as the hook's context. No token → prints nothing, so a plain
//!    `/clear` is untouched.
//!
//! `th harness budget` is the meter: hooked on Stop / PostToolUse /
//! UserPromptSubmit, it reads the session's context size (Claude Code: the
//! last main-thread assistant `usage` in `transcript_path`) and nudges once at
//! `harness.context_budget_warn`, then blocks the Stop once at
//! `harness.context_budget` so the agent runs th-clear before ending its turn.
//!
//! Every hook path is silent and exits 0 on any failure — a hook never breaks
//! the harness.

use std::fmt::Write as _;
use std::fs;
use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::pearls_handoff;

/// Default lifetime of an armed token: long enough to finish the handoff
/// turn and type `/clear`, short enough that a forgotten one doesn't resurrect
/// stale work tomorrow.
pub const DEFAULT_TTL_MINUTES: u32 = 15;
/// Per-pearl cap on the injected handoff text.
pub const PACKET_CHARS: usize = 6000;
/// Pearls injected per claim.
pub const MAX_PEARLS: usize = 3;
/// How much of the transcript tail the budget meter reads.
const TRANSCRIPT_TAIL_BYTES: u64 = 512 * 1024;
/// Process-tree hops to search for the owning harness.
const MAX_HOPS: usize = 16;

#[derive(Subcommand)]
pub enum HandoffCmd {
    /// Arm a one-shot resume for the next session in this terminal: the next
    /// SessionStart (after `/clear`, or a relaunch here) injects these pearls'
    /// handoff. Run after `th pearls checkpoint`; the th-clear skill does both.
    Arm {
        /// Pearl(s) to resume (repeatable).
        #[arg(long = "pearl", required = true)]
        pearls: Vec<String>,
        /// The harness this session runs in.
        #[arg(long, default_value = "claude-code")]
        harness: String,
        /// The outgoing session id (recorded for the trail).
        #[arg(long)]
        session_id: Option<String>,
        /// Directory the session runs in (default: cwd).
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long, default_value_t = DEFAULT_TTL_MINUTES)]
        ttl_minutes: u32,
    },
    /// SessionStart hook: take this terminal's armed token, if any, and print
    /// the handoff as session context. Reads the hook payload on stdin.
    /// Prints nothing (exit 0) when nothing is armed.
    Claim {
        #[arg(long, default_value = "claude-code")]
        harness: String,
        /// Directory (default: the payload's `cwd`, else cwd).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// `json` (Claude Code hookSpecificOutput) or `text`. Default: json for claude-code.
        #[arg(long)]
        format: Option<String>,
    },
    /// Show the armed tokens on this machine.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Drop the armed token(s) for a directory without resuming.
    Disarm {
        /// Directory (default: cwd).
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
}

/// One armed resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Armed {
    pub pearls: Vec<String>,
    pub cwd: String,
    pub harness: String,
    #[serde(default)]
    pub owner_pid: Option<u32>,
    /// Every PID this terminal's harness was known by at arm time (env and
    /// process-tree walk); a claim matches on any overlap.
    #[serde(default)]
    pub owner_pids: Vec<u32>,
    #[serde(default)]
    pub from_session: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Where SessionStart came from, as far as matching cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `/clear` (or `/new`): the same harness process, a new session.
    Clear,
    /// A fresh launch: a different process, so the PID can't match.
    Startup,
    /// compact / resume / unknown — never claims.
    Other,
}

impl Source {
    #[must_use]
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("") {
            "clear" | "new" => Self::Clear,
            "startup" | "" => Self::Startup,
            _ => Self::Other,
        }
    }
}

// ── storage ────────────────────────────────────────────────────────

/// `$SMOOTH_HOME` or `~/.smooth`.
fn smooth_dir() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("SMOOTH_HOME").filter(|h| !h.is_empty()) {
        return Ok(PathBuf::from(h));
    }
    Ok(dirs_next::home_dir().context("no home dir")?.join(".smooth"))
}

fn armed_dir() -> Result<PathBuf> {
    Ok(smooth_dir()?.join("handoff").join("armed"))
}

fn budget_dir() -> Result<PathBuf> {
    Ok(smooth_dir()?.join("handoff").join("budget"))
}

/// Canonical cwd string (symlinks resolved, so `/tmp` and `/private/tmp` agree).
fn canon(cwd: &Path) -> String {
    fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf()).to_string_lossy().into_owned()
}

/// Stable 16-hex key for a canonical cwd.
#[must_use]
pub fn cwd_key(canon_cwd: &str) -> String {
    let digest = Sha256::digest(canon_cwd.as_bytes());
    digest.iter().take(8).fold(String::with_capacity(16), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn token_name(key: &str, pid: Option<u32>) -> String {
    pid.map_or_else(|| format!("{key}-any.json"), |p| format!("{key}-{p}.json"))
}

/// Write `armed` into `dir` (tmp + rename). Re-arming the same terminal
/// replaces its token.
///
/// # Errors
/// Filesystem failures.
pub fn arm_in(dir: &Path, armed: &Armed) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(token_name(&cwd_key(&armed.cwd), armed.owner_pid));
    let tmp = dir.join(format!(".{}.tmp-{}", token_name(&cwd_key(&armed.cwd), armed.owner_pid), std::process::id()));
    fs::write(&tmp, serde_json::to_vec_pretty(armed)?).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(path)
}

/// Every parseable token in `dir` (path, token). Unparseable files are skipped.
fn tokens_in(dir: &Path) -> Vec<(PathBuf, Armed)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(PathBuf, Armed)> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let t: Armed = serde_json::from_slice(&fs::read(&p).ok()?).ok()?;
            Some((p, t))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Which of the fresh `candidates` (all for this cwd) this session may take.
/// Exact owner-PID match wins. Otherwise a single candidate is taken only
/// when the PIDs can't be compared (either side unknown) or this is a fresh
/// launch (`Startup`, where the PID necessarily differs). `Other` never claims.
#[must_use]
pub fn pick(candidates: &[Armed], owner_pids: &[u32], source: Source) -> Option<usize> {
    if source == Source::Other {
        return None;
    }
    let pids_of = |c: &Armed| -> Vec<u32> { c.owner_pids.iter().copied().chain(c.owner_pid).collect() };
    if let Some(i) = candidates.iter().position(|c| pids_of(c).iter().any(|p| owner_pids.contains(p))) {
        return Some(i);
    }
    if candidates.len() != 1 {
        return None;
    }
    let incomparable = owner_pids.is_empty() || pids_of(&candidates[0]).is_empty();
    (incomparable || source == Source::Startup).then_some(0)
}

/// Take this session's token for `cwd` from `dir`, if any: expired tokens for
/// the cwd are deleted on the way, and the take is an atomic rename so two
/// racing sessions can't both resume the same handoff.
pub fn claim_in(dir: &Path, canon_cwd: &str, owner_pids: &[u32], source: Source, now: DateTime<Utc>) -> Option<Armed> {
    let mut fresh: Vec<(PathBuf, Armed)> = Vec::new();
    for (path, t) in tokens_in(dir) {
        if t.cwd != canon_cwd {
            continue;
        }
        if t.expires_at <= now {
            let _ = fs::remove_file(&path);
            continue;
        }
        fresh.push((path, t));
    }
    let tokens: Vec<Armed> = fresh.iter().map(|(_, t)| t.clone()).collect();
    let i = pick(&tokens, owner_pids, source)?;
    let (path, token) = &fresh[i];
    let claimed = path.with_extension(format!("claimed-{}", std::process::id()));
    if fs::rename(path, &claimed).is_err() {
        return None; // another session won the race
    }
    let _ = fs::remove_file(&claimed);
    Some(token.clone())
}

// ── process tree ───────────────────────────────────────────────────

/// Executable names a harness runs as.
#[must_use]
pub fn harness_binaries(harness: &str) -> Vec<&str> {
    match harness {
        "claude-code" | "claude" => vec!["claude"],
        other => vec![other],
    }
}

/// Does a `ps args` line belong to one of `names`? Checks argv0 and argv1
/// (the script of a `node …/claude` launch) by basename.
#[must_use]
pub fn args_match(args: &str, names: &[&str]) -> bool {
    args.split_whitespace()
        .take(2)
        .any(|tok| names.iter().any(|n| Path::new(tok).file_name().is_some_and(|b| b == *n)))
}

/// Walk up from `start` (exclusive) via `parent_of(pid) -> (ppid, args)` to
/// the first ancestor whose args match `names`.
pub fn find_ancestor(start: u32, names: &[&str], parent_of: impl Fn(u32) -> Option<(u32, String)>) -> Option<u32> {
    let mut pid = parent_of(start)?.0;
    for _ in 0..MAX_HOPS {
        if pid <= 1 {
            return None;
        }
        let (parent, args) = parent_of(pid)?;
        if args_match(&args, names) {
            return Some(pid);
        }
        pid = parent;
    }
    None
}

fn ps_parent(pid: u32) -> Option<(u32, String)> {
    let out = Command::new("ps").args(["-o", "ppid=,args=", "-p", &pid.to_string()]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (parent, args) = line.split_once(char::is_whitespace)?;
    Some((parent.trim().parse().ok()?, args.trim().to_string()))
}

/// The PIDs of the harness process this command runs under, best first:
/// `SMOOTH_HARNESS_PID` alone when set (tests, harnesses that know their own
/// PID); else the harness's own env (`CLAUDE_PID` in Claude Code) plus a
/// process-tree walk. Both are kept because the tool shell and the hook
/// process may not agree on which one they see.
#[must_use]
pub fn owner_pids(harness: &str) -> Vec<u32> {
    let env_pid = |k: &str| std::env::var(k).ok().and_then(|p| p.trim().parse::<u32>().ok()).filter(|p| *p > 1);
    if let Some(p) = env_pid("SMOOTH_HARNESS_PID") {
        return vec![p];
    }
    let mut pids = Vec::new();
    if matches!(harness, "claude-code" | "claude") {
        pids.extend(env_pid("CLAUDE_PID"));
    }
    if let Some(p) = find_ancestor(std::process::id(), &harness_binaries(harness), ps_parent) {
        if !pids.contains(&p) {
            pids.push(p);
        }
    }
    pids
}

// ── hook payload ───────────────────────────────────────────────────

/// The harness hook payload on stdin, or `Null` for an interactive terminal.
fn read_payload() -> Value {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Value::Null;
    }
    let mut s = String::new();
    let _ = stdin.lock().read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

fn pstr<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

// ── claim output ───────────────────────────────────────────────────

/// The context a claimed resume injects.
#[must_use]
pub fn resume_context(packets: &[(String, String)]) -> String {
    let mut out = String::from(
        "Resumed via /th-clear: the previous session in this terminal handed this work off and cleared its context. \
         Continue it. The handoff is below — re-verify live state (git status, PRs, CI) before acting, then carry on from `next`. \
         Checkpoint with `th pearls checkpoint <id> --note \"…\" --next \"…\"`.\n",
    );
    for (id, text) in packets {
        out.push('\n');
        if text.chars().count() > PACKET_CHARS {
            out.extend(text.chars().take(PACKET_CHARS));
            let _ = write!(out, "\n… (truncated — th pearls show {id} --handoff)\n");
        } else {
            out.push_str(text);
        }
    }
    out
}

/// Emit `context` for a SessionStart hook in `format`.
#[must_use]
pub fn session_start_output(context: &str, format: &str) -> String {
    if format == "text" {
        context.to_string()
    } else {
        json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": context}}).to_string()
    }
}

fn default_format(harness: &str) -> &'static str {
    if matches!(harness, "claude-code" | "claude") {
        "json"
    } else {
        "text"
    }
}

fn do_claim(harness: &str, cwd: Option<PathBuf>, format: Option<String>) -> Result<Option<String>> {
    let payload = read_payload();
    let cwd = cwd
        .or_else(|| pstr(&payload, "cwd").map(PathBuf::from))
        .map_or_else(std::env::current_dir, Ok)?;
    let source = Source::parse(pstr(&payload, "source"));
    let session_id = pstr(&payload, "session_id").map(str::to_string);
    let Some(token) = claim_in(&armed_dir()?, &canon(&cwd), &owner_pids(harness), source, Utc::now()) else {
        return Ok(None);
    };
    let store = smooth_pearls::PearlStore::open(&cwd)?;
    let mut packets = Vec::new();
    for id in token.pearls.iter().take(MAX_PEARLS) {
        let Some(pearl) = store.get(id)? else { continue };
        // Re-stamp with the new session id so PreCompact / handoff-context
        // keep matching this pearl in a primary checkout.
        let opts = pearls_handoff::CheckpointOpts {
            note: Some(format!(
                "resumed via /th-clear{}",
                session_id.as_deref().map(|s| format!(" in session {s}")).unwrap_or_default()
            )),
            next: None,
            auto: true,
            session_id: session_id.clone(),
        };
        let _ = pearls_handoff::checkpoint(&store, id, &cwd, &opts);
        let packet = pearls_handoff::packet(&store, &pearl, false)?;
        packets.push((id.clone(), pearls_handoff::render(&packet)));
    }
    if packets.is_empty() {
        return Ok(None);
    }
    let fmt = format.unwrap_or_else(|| default_format(harness).to_string());
    Ok(Some(session_start_output(&resume_context(&packets), &fmt)))
}

/// # Errors
/// `arm` fails loudly (unknown pearl, unwritable dir); `claim` never does.
pub fn handoff_cmd(cmd: HandoffCmd) -> Result<()> {
    match cmd {
        HandoffCmd::Arm {
            pearls,
            harness,
            session_id,
            cwd,
            ttl_minutes,
        } => {
            let cwd = cwd.map_or_else(std::env::current_dir, Ok)?;
            let store = smooth_pearls::PearlStore::open(&cwd)?;
            for id in &pearls {
                if store.get(id)?.is_none() {
                    bail!("pearl not found: {id}");
                }
                if pearls_handoff::parse_checkpoints(&store.get_comments(id)?).is_empty() {
                    eprintln!("warning: {id} has no checkpoint yet — run `th pearls checkpoint {id} --note … --next …` first");
                }
            }
            let now = Utc::now();
            let pids = owner_pids(&harness);
            let owner = pids.first().copied();
            let armed = Armed {
                pearls,
                cwd: canon(&cwd),
                harness: harness.clone(),
                owner_pid: owner,
                owner_pids: pids,
                from_session: session_id
                    .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok())
                    .or_else(|| std::env::var("CLAUDE_SESSION_ID").ok())
                    .filter(|s| !s.is_empty()),
                created_at: now,
                expires_at: now + Duration::minutes(i64::from(ttl_minutes)),
            };
            arm_in(&armed_dir()?, &armed)?;
            let clear = if matches!(harness.as_str(), "claude-code" | "claude") {
                "/clear"
            } else {
                "your harness's clear (/clear or /new)"
            };
            println!(
                "✓ Armed {} for {} (expires in {ttl_minutes}m{}).\n  Now type {clear} — the next session here resumes it automatically.",
                armed.pearls.join(", "),
                armed.cwd,
                owner.map(|p| format!(", harness pid {p}")).unwrap_or_default()
            );
            Ok(())
        }
        HandoffCmd::Claim { harness, cwd, format } => {
            if let Ok(Some(out)) = do_claim(&harness, cwd, format) {
                println!("{out}");
            }
            Ok(())
        }
        HandoffCmd::List { json } => {
            let tokens: Vec<Armed> = tokens_in(&armed_dir()?).into_iter().map(|(_, t)| t).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&tokens)?);
            } else if tokens.is_empty() {
                println!("Nothing armed.");
            } else {
                let now = Utc::now();
                for t in tokens {
                    let left = (t.expires_at - now).num_minutes();
                    let state = if left < 0 { "expired".to_string() } else { format!("{left}m left") };
                    println!("{}  {}  {}  {state}", t.pearls.join(","), t.harness, t.cwd);
                }
            }
            Ok(())
        }
        HandoffCmd::Disarm { cwd } => {
            let cwd = canon(&cwd.map_or_else(std::env::current_dir, Ok)?);
            let mut n = 0;
            for (path, t) in tokens_in(&armed_dir()?) {
                if t.cwd == cwd && fs::remove_file(&path).is_ok() {
                    n += 1;
                }
            }
            println!("Disarmed {n} token(s) for {cwd}.");
            Ok(())
        }
    }
}

// ── budget ─────────────────────────────────────────────────────────

/// Context size of the latest main-thread assistant turn in a Claude Code
/// transcript (JSONL): input + cache-creation + cache-read tokens. Reads only
/// the tail of the file. `None` when nothing usable is found.
#[must_use]
pub fn transcript_context_tokens(path: &Path) -> Option<u64> {
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // partial first line
    }
    lines.iter().rev().find_map(|l| usage_tokens(l))
}

/// Context tokens from one transcript line, when it is a main-thread
/// assistant message carrying `usage`.
#[must_use]
pub fn usage_tokens(line: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let u = v.get("message")?.get("usage")?;
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let total = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
    (total > 0).then_some(total)
}

/// One-shot flags per session, so each nudge fires once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetState {
    #[serde(default)]
    pub warned: bool,
    #[serde(default)]
    pub over_noted: bool,
    #[serde(default)]
    pub blocked: bool,
}

/// What the budget hook says for one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Quiet,
    /// Inject as context (PostToolUse / UserPromptSubmit).
    Context(String),
    /// Block the Stop with this reason.
    Block(String),
}

fn k(n: u64) -> String {
    format!("{}k", n / 1000)
}

/// Pure decision: `(verdict, next_state)`. `hard`/`warn` of 0 disable.
#[must_use]
pub fn decide(event: &str, tokens: u64, warn: u64, hard: u64, state: &BudgetState, stop_hook_active: bool) -> (Verdict, BudgetState) {
    let mut next = state.clone();
    let over = hard > 0 && tokens >= hard;
    let near = warn > 0 && tokens >= warn;
    let over_msg = format!(
        "Context is at {} tokens, over this session's {} budget (harness.context_budget). Hand off now: finish the current step, \
         invoke the th-clear skill (it checkpoints the work into a pearl and arms the resume), then stop and tell the user to type /clear.",
        k(tokens),
        k(hard)
    );
    let verdict = match event {
        "Stop" if over && !state.blocked && !stop_hook_active => {
            next.blocked = true;
            next.over_noted = true;
            Verdict::Block(over_msg)
        }
        "PostToolUse" | "UserPromptSubmit" if over && !state.over_noted => {
            next.over_noted = true;
            next.warned = true;
            Verdict::Context(over_msg)
        }
        "UserPromptSubmit" | "PostToolUse" if near && !over && !state.warned => {
            next.warned = true;
            Verdict::Context(format!(
                "Context is at {} tokens; this session's budget is {} (harness.context_budget). Wrap up the current piece of work — \
                 at a clean stopping point, invoke the th-clear skill to hand off and continue in a fresh context.",
                k(tokens),
                k(hard.max(warn))
            ))
        }
        _ => Verdict::Quiet,
    };
    (verdict, next)
}

/// Render a verdict as hook stdout for `event`.
#[must_use]
pub fn verdict_output(v: &Verdict, event: &str, format: &str) -> Option<String> {
    match v {
        Verdict::Quiet => None,
        Verdict::Block(reason) if format == "text" => Some(reason.clone()),
        Verdict::Block(reason) => Some(json!({"decision": "block", "reason": reason}).to_string()),
        Verdict::Context(msg) if format == "text" => Some(msg.clone()),
        Verdict::Context(msg) => Some(json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": msg}}).to_string()),
    }
}

/// The effective value of an int setting (env > file > registry default);
/// unset, unparseable or negative → 0 (disabled).
fn setting_u64(key: &str) -> u64 {
    setting_u64_from(&smooth_policy::settings::Resolver::from_process(), key)
}

fn setting_u64_from(resolver: &smooth_policy::settings::Resolver, key: &str) -> u64 {
    resolver
        .resolve(key)
        .ok()
        .and_then(|r| r.value)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map_or(0, |n| u64::try_from(n).unwrap_or(0))
}

fn safe_id(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(80).collect()
}

/// `th harness budget`: the hook entry. Silent unless a nudge is due; never fails.
pub fn budget_cmd(harness: &str, event: Option<String>, tokens: Option<u64>, format: Option<String>) {
    let payload = read_payload();
    let warn = setting_u64("harness.context_budget_warn");
    let hard = setting_u64("harness.context_budget");
    let event = event.or_else(|| pstr(&payload, "hook_event_name").map(str::to_string));
    let tokens = tokens.or_else(|| pstr(&payload, "transcript_path").and_then(|p| transcript_context_tokens(Path::new(p))));
    let Some(event) = event else {
        println!(
            "budget {} · warn {} · current {}",
            if hard == 0 { "off".into() } else { k(hard) },
            if warn == 0 { "off".into() } else { k(warn) },
            tokens.map_or_else(|| "unknown (no hook payload)".into(), k)
        );
        return;
    };
    let Some(tokens) = tokens else { return };
    let session = pstr(&payload, "session_id").map(safe_id).filter(|s| !s.is_empty());
    let state_path = session.as_ref().and_then(|s| budget_dir().ok().map(|d| d.join(format!("{s}.json"))));
    let state: BudgetState = state_path
        .as_ref()
        .and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let stop_active = payload.get("stop_hook_active").and_then(Value::as_bool).unwrap_or(false);
    let (verdict, next) = decide(&event, tokens, warn, hard, &state, stop_active);
    if next != state {
        if let Some(p) = &state_path {
            if let Some(dir) = p.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(p, serde_json::to_vec(&next).unwrap_or_default());
        }
    }
    let fmt = format.unwrap_or_else(|| default_format(harness).to_string());
    if let Some(out) = verdict_output(&verdict, &event, &fmt) {
        println!("{out}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn token(cwd: &str, pid: Option<u32>, mins: i64, now: DateTime<Utc>) -> Armed {
        Armed {
            pearls: vec!["th-abc123".into()],
            cwd: cwd.into(),
            harness: "claude-code".into(),
            owner_pid: pid,
            owner_pids: pid.into_iter().collect(),
            from_session: Some("old".into()),
            created_at: now,
            expires_at: now + Duration::minutes(mins),
        }
    }

    #[test]
    fn source_parse() {
        assert_eq!(Source::parse(Some("clear")), Source::Clear);
        assert_eq!(Source::parse(Some("new")), Source::Clear);
        assert_eq!(Source::parse(Some("startup")), Source::Startup);
        assert_eq!(Source::parse(None), Source::Startup);
        assert_eq!(Source::parse(Some("compact")), Source::Other);
        assert_eq!(Source::parse(Some("resume")), Source::Other);
    }

    #[test]
    fn cwd_key_is_stable_and_distinct() {
        assert_eq!(cwd_key("/a/b"), cwd_key("/a/b"));
        assert_ne!(cwd_key("/a/b"), cwd_key("/a/c"));
        assert_eq!(cwd_key("/a/b").len(), 16);
    }

    #[test]
    fn pick_prefers_exact_pid() {
        let now = Utc::now();
        let c = vec![token("/w", Some(10), 5, now), token("/w", Some(20), 5, now)];
        assert_eq!(pick(&c, &[20], Source::Clear), Some(1));
    }

    #[test]
    fn pick_refuses_another_terminals_token_on_clear() {
        let now = Utc::now();
        let c = vec![token("/w", Some(10), 5, now)];
        assert_eq!(pick(&c, &[99], Source::Clear), None);
    }

    #[test]
    fn pick_takes_single_token_on_startup_or_unknown_pid() {
        let now = Utc::now();
        let c = vec![token("/w", Some(10), 5, now)];
        assert_eq!(pick(&c, &[99], Source::Startup), Some(0));
        assert_eq!(pick(&c, &[], Source::Clear), Some(0));
        let anon = vec![token("/w", None, 5, now)];
        assert_eq!(pick(&anon, &[99], Source::Clear), Some(0));
    }

    #[test]
    fn pick_matches_any_recorded_pid() {
        let now = Utc::now();
        let mut t = token("/w", Some(10), 5, now);
        t.owner_pids = vec![10, 11];
        assert_eq!(pick(&[t.clone()], &[11], Source::Clear), Some(0));
        assert_eq!(pick(&[t], &[99, 10], Source::Clear), Some(0));
    }

    #[test]
    fn old_tokens_without_owner_pids_still_match() {
        let now = Utc::now();
        let mut t = token("/w", Some(10), 5, now);
        t.owner_pids.clear();
        assert_eq!(pick(&[t], &[10], Source::Clear), Some(0));
    }

    #[test]
    fn pick_is_ambiguous_with_two_foreign_tokens() {
        let now = Utc::now();
        let c = vec![token("/w", Some(10), 5, now), token("/w", Some(20), 5, now)];
        assert_eq!(pick(&c, &[99], Source::Startup), None);
        assert_eq!(pick(&c, &[], Source::Clear), None);
    }

    #[test]
    fn pick_never_claims_on_compact_or_resume() {
        let now = Utc::now();
        let c = vec![token("/w", Some(10), 5, now)];
        assert_eq!(pick(&c, &[10], Source::Other), None);
    }

    #[test]
    fn arm_then_claim_is_one_shot() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), 5, now)).unwrap();
        let got = claim_in(dir.path(), "/w", &[10], Source::Clear, now);
        assert_eq!(got.unwrap().pearls, vec!["th-abc123".to_string()]);
        assert!(claim_in(dir.path(), "/w", &[10], Source::Clear, now).is_none());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "claimed token is gone");
    }

    #[test]
    fn claim_ignores_other_cwds_and_reaps_expired() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/other", Some(10), 5, now)).unwrap();
        arm_in(dir.path(), &token("/w", Some(10), -1, now)).unwrap();
        assert!(claim_in(dir.path(), "/w", &[10], Source::Clear, now).is_none());
        let left: Vec<Armed> = tokens_in(dir.path()).into_iter().map(|(_, t)| t).collect();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].cwd, "/other");
    }

    #[test]
    fn rearm_replaces_same_terminal_token() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), 5, now)).unwrap();
        let mut t = token("/w", Some(10), 5, now);
        t.pearls = vec!["th-def456".into()];
        arm_in(dir.path(), &t).unwrap();
        assert_eq!(tokens_in(dir.path()).len(), 1);
        let got = claim_in(dir.path(), "/w", &[10], Source::Clear, now).unwrap();
        assert_eq!(got.pearls, vec!["th-def456".to_string()]);
    }

    #[test]
    fn claim_with_missing_dir_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(claim_in(&missing, "/w", &[], Source::Clear, Utc::now()).is_none());
    }

    #[test]
    fn args_match_by_basename() {
        assert!(args_match("/Users/x/.local/bin/claude --resume", &["claude"]));
        assert!(args_match("claude", &["claude"]));
        assert!(args_match("node /opt/lib/node_modules/.bin/claude", &["claude"]));
        assert!(!args_match("/bin/zsh -c th harness", &["claude"]));
        assert!(!args_match("claude-helper", &["claude"]));
        assert!(!args_match("", &["claude"]));
    }

    #[test]
    fn find_ancestor_walks_to_harness() {
        // 100 (th) → 50 (zsh) → 40 (claude) → 1
        let tree: HashMap<u32, (u32, String)> = [
            (100, (50, "th harness handoff arm".to_string())),
            (50, (40, "/bin/zsh -c th".to_string())),
            (40, (1, "/Users/x/.local/bin/claude".to_string())),
        ]
        .into_iter()
        .collect();
        assert_eq!(find_ancestor(100, &["claude"], |p| tree.get(&p).cloned()), Some(40));
        assert_eq!(find_ancestor(100, &["codex"], |p| tree.get(&p).cloned()), None);
        assert_eq!(find_ancestor(7, &["claude"], |p| tree.get(&p).cloned()), None);
    }

    #[test]
    fn harness_binaries_maps_claude_code() {
        assert_eq!(harness_binaries("claude-code"), vec!["claude"]);
        assert_eq!(harness_binaries("codex"), vec!["codex"]);
    }

    #[test]
    fn resume_context_truncates_long_packets() {
        let long = "x".repeat(PACKET_CHARS + 50);
        let out = resume_context(&[("th-1".into(), long), ("th-2".into(), "short".into())]);
        assert!(out.starts_with("Resumed via /th-clear"));
        assert!(out.contains("truncated — th pearls show th-1 --handoff"));
        assert!(out.contains("short"));
    }

    #[test]
    fn session_start_output_formats() {
        let j: Value = serde_json::from_str(&session_start_output("ctx", "json")).unwrap();
        assert_eq!(j["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(j["hookSpecificOutput"]["additionalContext"], "ctx");
        assert_eq!(session_start_output("ctx", "text"), "ctx");
    }

    #[test]
    fn usage_tokens_sums_main_thread_assistant_usage() {
        let l = r#"{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_creation_input_tokens":200,"cache_read_input_tokens":3000,"output_tokens":99}}}"#;
        assert_eq!(usage_tokens(l), Some(3210));
        let side = r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":10}}}"#;
        assert_eq!(usage_tokens(side), None);
        assert_eq!(usage_tokens(r#"{"type":"user","message":{}}"#), None);
        assert_eq!(usage_tokens("not json"), None);
        assert_eq!(usage_tokens(r#"{"type":"assistant","message":{"usage":{}}}"#), None);
    }

    #[test]
    fn transcript_context_tokens_reads_latest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let lines = [
            r#"{"type":"assistant","message":{"usage":{"input_tokens":1,"cache_read_input_tokens":1000}}}"#,
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"assistant","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":5000}}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":9,"cache_read_input_tokens":90000}}}"#,
        ];
        fs::write(&p, lines.join("\n")).unwrap();
        assert_eq!(transcript_context_tokens(&p), Some(5002));
        assert_eq!(transcript_context_tokens(&dir.path().join("missing.jsonl")), None);
    }

    #[test]
    fn transcript_context_tokens_handles_large_file_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.jsonl");
        let filler = format!(r#"{{"type":"user","message":{{"content":"{}"}}}}"#, "y".repeat(1000));
        let mut body = String::new();
        body.push_str(r#"{"type":"assistant","message":{"usage":{"input_tokens":1,"cache_read_input_tokens":7}}}"#);
        body.push('\n');
        for _ in 0..800 {
            body.push_str(&filler);
            body.push('\n');
        }
        body.push_str(r#"{"type":"assistant","message":{"usage":{"input_tokens":4,"cache_read_input_tokens":240000}}}"#);
        fs::write(&p, body).unwrap();
        assert_eq!(transcript_context_tokens(&p), Some(240_004));
    }

    #[test]
    fn decide_quiet_under_warn() {
        let (v, s) = decide("Stop", 100_000, 220_000, 250_000, &BudgetState::default(), false);
        assert_eq!(v, Verdict::Quiet);
        assert_eq!(s, BudgetState::default());
    }

    #[test]
    fn decide_warns_once_between_warn_and_hard() {
        let (v, s) = decide("UserPromptSubmit", 230_000, 220_000, 250_000, &BudgetState::default(), false);
        assert!(matches!(v, Verdict::Context(ref m) if m.contains("230k") && m.contains("th-clear")));
        assert!(s.warned);
        let (v2, _) = decide("PostToolUse", 235_000, 220_000, 250_000, &s, false);
        assert_eq!(v2, Verdict::Quiet);
        // Stop never warns, only blocks.
        let (v3, _) = decide("Stop", 230_000, 220_000, 250_000, &BudgetState::default(), false);
        assert_eq!(v3, Verdict::Quiet);
    }

    #[test]
    fn decide_blocks_stop_once_over_hard() {
        let (v, s) = decide("Stop", 251_000, 220_000, 250_000, &BudgetState::default(), false);
        assert!(matches!(v, Verdict::Block(ref m) if m.contains("251k") && m.contains("/clear")));
        assert!(s.blocked);
        let (v2, _) = decide("Stop", 260_000, 220_000, 250_000, &s, false);
        assert_eq!(v2, Verdict::Quiet, "blocks only once");
    }

    #[test]
    fn decide_respects_stop_hook_active() {
        let (v, s) = decide("Stop", 300_000, 220_000, 250_000, &BudgetState::default(), true);
        assert_eq!(v, Verdict::Quiet);
        assert!(!s.blocked);
    }

    #[test]
    fn decide_notes_over_budget_mid_turn_once() {
        let (v, s) = decide("PostToolUse", 255_000, 220_000, 250_000, &BudgetState::default(), false);
        assert!(matches!(v, Verdict::Context(_)));
        assert!(s.over_noted && s.warned && !s.blocked);
        let (v2, _) = decide("PostToolUse", 270_000, 220_000, 250_000, &s, false);
        assert_eq!(v2, Verdict::Quiet);
        // The Stop still blocks after a mid-turn note.
        let (v3, _) = decide("Stop", 270_000, 220_000, 250_000, &s, false);
        assert!(matches!(v3, Verdict::Block(_)));
    }

    #[test]
    fn decide_disabled_by_zero() {
        let (v, _) = decide("Stop", 900_000, 0, 0, &BudgetState::default(), false);
        assert_eq!(v, Verdict::Quiet);
        let (v, _) = decide("UserPromptSubmit", 900_000, 0, 250_000, &BudgetState::default(), false);
        assert!(matches!(v, Verdict::Context(_)), "hard still notes when warn is off");
    }

    #[test]
    fn verdict_output_shapes() {
        let b: Value = serde_json::from_str(&verdict_output(&Verdict::Block("r".into()), "Stop", "json").unwrap()).unwrap();
        assert_eq!(b["decision"], "block");
        assert_eq!(b["reason"], "r");
        let c: Value = serde_json::from_str(&verdict_output(&Verdict::Context("m".into()), "PostToolUse", "json").unwrap()).unwrap();
        assert_eq!(c["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(c["hookSpecificOutput"]["additionalContext"], "m");
        assert_eq!(verdict_output(&Verdict::Context("m".into()), "PostToolUse", "text").as_deref(), Some("m"));
        assert!(verdict_output(&Verdict::Quiet, "Stop", "json").is_none());
    }

    #[test]
    fn budget_settings_fall_back_to_registry_defaults() {
        use smooth_policy::settings::{Resolver, SettingsFile};
        let none = Resolver::new(|_| None, SettingsFile::empty());
        assert_eq!(setting_u64_from(&none, "harness.context_budget"), 250_000);
        assert_eq!(setting_u64_from(&none, "harness.context_budget_warn"), 220_000);
        let env = Resolver::new(
            |k| match k {
                "SMOOTH_CONTEXT_BUDGET" => Some("0".into()),
                "SMOOTH_CONTEXT_BUDGET_WARN" => Some("-5".into()),
                _ => None,
            },
            SettingsFile::empty(),
        );
        assert_eq!(setting_u64_from(&env, "harness.context_budget"), 0, "0 disables");
        assert_eq!(setting_u64_from(&env, "harness.context_budget_warn"), 0, "negative is disabled");
    }

    #[test]
    fn safe_id_strips_path_chars() {
        assert_eq!(safe_id("../../etc/passwd"), "etcpasswd");
        assert_eq!(safe_id("abc-123_X"), "abc-123_X");
    }
}
