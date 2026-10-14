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
//!    apart). The owning terminal can claim it for a day; any other launch
//!    only within `--ttl-minutes`. It also prints the resume prompt and copies
//!    it to the clipboard, so a missed claim is one paste away.
//! 2. The user types the harness's clear (`/clear`). Its SessionStart hook runs
//!    `th harness handoff claim`, which takes the token (atomic rename — one
//!    claimer wins), re-stamps each pearl with the NEW session id and prints
//!    the handoff as the hook's context. No token → prints nothing, so a plain
//!    `/clear` is untouched. A token for this cwd that it may not take
//!    (expired, another terminal's) becomes a one-line note — never silence.
//!
//! `th harness budget` is the meter: hooked on PostToolUse / UserPromptSubmit,
//! it reads the session's context size (Claude Code: the last main-thread
//! assistant `usage` in `transcript_path`) and only ever adds context — it
//! never blocks. Past `harness.context_suggest`, a milestone (commit, push,
//! attest, PR, pearl checkpoint, green tests) asks the agent to suggest
//! `/th-clear` to the user; `harness.context_budget_warn` says the reset is
//! near. `--statusline` prints the context segment for the statusline.
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

use crate::harness_compact::{Budgets, Reset};
use crate::pearls_handoff;

/// Default lifetime of an armed token for a claim that can't prove it is the
/// arming terminal (a fresh launch, unknown PIDs).
pub const DEFAULT_TTL_MINUTES: u32 = 60;
/// How long past `expires_at` the arming terminal itself can still claim: the
/// PID match is what makes a claim safe, so a `/clear` typed an hour after
/// arming must still resume (SMOODEV-3888). Past this the token is reaped.
pub const SAME_TERMINAL_GRACE_HOURS: i64 = 24;
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
        /// Don't copy the resume prompt to the clipboard.
        #[arg(long)]
        no_copy: bool,
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

/// What a SessionStart claim found for its cwd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// This session took the token.
    Resumed(Armed),
    /// Tokens exist for this cwd, but none was this session's to take.
    Missed(Vec<Missed>),
    /// Nothing armed here (or a compact/resume, which never claims).
    Nothing,
}

/// A token this session saw but did not take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missed {
    pub token: Armed,
    /// Past `expires_at` (otherwise: another terminal's, or ambiguous).
    pub expired: bool,
}

fn pids_of(t: &Armed) -> Vec<u32> {
    t.owner_pids.iter().copied().chain(t.owner_pid).collect()
}

/// Take this session's token for `cwd` from `dir`. The arming terminal (an
/// owner-PID match) claims even past `expires_at`, up to
/// [`SAME_TERMINAL_GRACE_HOURS`]; everything else goes through [`pick`] over
/// unexpired tokens only. The take is an atomic rename so two racing sessions
/// can't both resume one handoff. Tokens left behind come back as
/// [`Claim::Missed`]; an expired one whose terminal is gone (`alive` says no
/// for every owner PID) is reaped once reported, since nothing can claim it.
pub fn claim_in(dir: &Path, canon_cwd: &str, owner_pids: &[u32], source: Source, now: DateTime<Utc>, alive: impl Fn(u32) -> bool) -> Claim {
    if source == Source::Other {
        return Claim::Nothing;
    }
    let grace = Duration::hours(SAME_TERMINAL_GRACE_HOURS);
    let mut live: Vec<(PathBuf, Armed)> = Vec::new();
    for (path, t) in tokens_in(dir) {
        if t.cwd != canon_cwd {
            continue;
        }
        if t.expires_at + grace <= now {
            let _ = fs::remove_file(&path);
            continue;
        }
        live.push((path, t));
    }
    let own = live.iter().position(|(_, t)| pids_of(t).iter().any(|p| owner_pids.contains(p)));
    let chosen = own.or_else(|| {
        let fresh: Vec<usize> = (0..live.len()).filter(|&i| live[i].1.expires_at > now).collect();
        let tokens: Vec<Armed> = fresh.iter().map(|&i| live[i].1.clone()).collect();
        pick(&tokens, owner_pids, source).map(|i| fresh[i])
    });
    if let Some(i) = chosen {
        let (path, token) = &live[i];
        let claimed = path.with_extension(format!("claimed-{}", std::process::id()));
        if fs::rename(path, &claimed).is_err() {
            return Claim::Nothing; // another session won the race
        }
        let _ = fs::remove_file(&claimed);
        return Claim::Resumed(token.clone());
    }
    if live.is_empty() {
        return Claim::Nothing;
    }
    let missed = live
        .into_iter()
        .map(|(path, token)| {
            let expired = token.expires_at <= now;
            if expired && !pids_of(&token).into_iter().any(&alive) {
                let _ = fs::remove_file(&path);
            }
            Missed { token, expired }
        })
        .collect();
    Claim::Missed(missed)
}

/// Whether `pid` is a running process.
fn pid_alive(pid: u32) -> bool {
    ps_parent(pid).is_some()
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

/// The context for tokens a claim saw but did not take, so the fresh session
/// knows a handoff exists instead of starting cold without a word.
#[must_use]
pub fn missed_context(missed: &[Missed], now: DateTime<Utc>) -> String {
    let mut out = String::from("A /th-clear handoff is armed for this directory but was not resumed automatically:\n");
    for m in missed {
        let age = (now - m.token.created_at).num_minutes().max(0);
        let why = if m.expired { "expired" } else { "armed by another terminal" };
        let _ = writeln!(out, "- {} (armed {age}m ago, {why})", m.token.pearls.join(", "));
    }
    out.push_str(
        "If the user's message is about continuing that work, run `th pearls show <id> --handoff`, re-verify live state, \
         and carry on from its `next`; otherwise ignore this.",
    );
    out
}

/// Best-effort copy to the system clipboard; `false` when no tool worked.
fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write as _;
    use std::process::Stdio;
    let tools: &[(&str, &[&str])] = &[("pbcopy", &[]), ("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("clip.exe", &[])];
    tools.iter().any(|(bin, args)| {
        let Ok(mut child) = Command::new(bin)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let wrote = child.stdin.take().is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        child.wait().is_ok_and(|st| st.success()) && wrote
    })
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
    let now = Utc::now();
    let fmt = format.unwrap_or_else(|| default_format(harness).to_string());
    let token = match claim_in(&armed_dir()?, &canon(&cwd), &owner_pids(harness), source, now, pid_alive) {
        Claim::Resumed(token) => token,
        Claim::Missed(missed) => return Ok(Some(session_start_output(&missed_context(&missed, now), &fmt))),
        Claim::Nothing => return Ok(None),
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
            no_copy,
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
            let window = owner.map_or_else(
                || format!("expires in {ttl_minutes}m"),
                |p| format!("this terminal (harness pid {p}) for {SAME_TERMINAL_GRACE_HOURS}h, a new launch here for {ttl_minutes}m"),
            );
            let copied = !no_copy && copy_to_clipboard(&resume_commands(&harness, &armed.pearls));
            println!(
                "✓ Armed {} for {} ({window}).\n{}{}",
                armed.pearls.join(", "),
                armed.cwd,
                resume_instructions(&harness, &armed.pearls),
                if copied { "\n  (the resume line is on your clipboard)" } else { "" }
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

/// What the user types after arming: the harness's clear, then the explicit
/// resume command. The armed token usually resumes on its own after the clear;
/// the resume line is the copy-pasteable fallback when it doesn't.
#[must_use]
pub fn resume_instructions(harness: &str, pearls: &[String]) -> String {
    let (clear, _) = clear_and_skill(harness);
    let resume: Vec<String> = resume_commands(harness, pearls).lines().map(|l| format!("    {l}")).collect();
    format!("  Type:\n    {clear}\n  then:\n{}", resume.join("\n"))
}

/// The harness's clear and the th-handoff skill's invocation in it.
const fn clear_and_skill(harness: &str) -> (&'static str, &'static str) {
    match harness.as_bytes() {
        b"claude-code" | b"claude" => ("/clear", "/th-handoff"),
        b"codex" => ("/new", "$th-handoff"),
        _ => ("/clear (or /new)", "/th-handoff"),
    }
}

/// Just the resume line(s), one per pearl: what `arm` puts on the clipboard.
#[must_use]
pub fn resume_commands(harness: &str, pearls: &[String]) -> String {
    let (_, skill) = clear_and_skill(harness);
    pearls.iter().map(|p| format!("{skill} resume {p}")).collect::<Vec<_>>().join("\n")
}

// ── budget ─────────────────────────────────────────────────────────

/// Context size of the latest main-thread assistant turn in a Claude Code
/// transcript (JSONL) — input + cache-creation + cache-read tokens — and the
/// model it ran on (for per-model budgets). Reads only the tail of the file.
/// `None` when nothing usable is found.
#[must_use]
pub fn transcript_context(path: &Path) -> Option<(u64, Option<String>)> {
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
    lines.iter().rev().find_map(|l| usage_context(l))
}

/// Context tokens (and `model`) from one transcript line, when it is a
/// main-thread assistant message carrying `usage`.
#[must_use]
pub fn usage_context(line: &str) -> Option<(u64, Option<String>)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let msg = v.get("message")?;
    let u = msg.get("usage")?;
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let total = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
    let model = msg
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty() && !m.starts_with('<'))
        .map(str::to_string);
    (total > 0).then_some((total, model))
}

/// Per-session flags, so each nudge fires once per segment (a compaction
/// starts a new segment).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetState {
    #[serde(default)]
    pub warned: bool,
    #[serde(default)]
    pub over_noted: bool,
    /// Context at the last `/th-clear` suggestion; 0 = none this segment.
    #[serde(default)]
    pub suggested_at: u64,
    /// Largest context seen since a flag was set. A context under half of it
    /// means the session compacted, which re-arms every nudge.
    #[serde(default)]
    pub peak: u64,
}

impl BudgetState {
    const fn flagged(&self) -> bool {
        self.warned || self.over_noted || self.suggested_at > 0
    }
}

/// What the budget hook says for one event. It only ever adds context —
/// nothing blocks: the user decides when to `/th-clear`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Quiet,
    /// Inject as context (PostToolUse / UserPromptSubmit).
    Context(String),
}

/// After a suggestion, the next one waits for this much more context.
pub const SUGGEST_STEP: u64 = 50_000;

fn k(n: u64) -> String {
    format!("{}k", n / 1000)
}

/// The shell command a PostToolUse payload ran, if the tool was a shell.
fn shell_command(payload: &Value) -> Option<String> {
    let input = payload.get("tool_input")?;
    match input.get("command").or_else(|| input.get("cmd"))? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")),
        _ => None,
    }
}

/// The tool's output as text (Claude Code: `{stdout, stderr, …}`; others: a string).
fn tool_output(payload: &Value) -> String {
    match payload.get("tool_response").or_else(|| payload.get("tool_output")) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => ["stdout", "stderr", "output"]
            .iter()
            .filter_map(|k| o.get(*k).and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Did a test run's output report a failure? (`FAILED`/`FAIL`, or `N failed` with N > 0.)
#[must_use]
pub fn output_failed(out: &str) -> bool {
    if out.contains("FAIL") {
        return true;
    }
    out.match_indices(" failed").any(|(i, _)| {
        let head = out[..i].trim_end_matches(' ');
        let digits: String = head.chars().rev().take_while(char::is_ascii_digit).collect();
        digits.chars().any(|c| c != '0')
    })
}

/// Milestones: shell commands that end a piece of work, as consecutive words.
const MILESTONES: &[(&[&str], &str)] = &[
    (&["git", "commit"], "a commit"),
    (&["git", "push"], "a push"),
    (&["th", "attest"], "th attest"),
    (&["gh", "pr", "create"], "opening a PR"),
    (&["gh", "pr", "merge"], "a merge"),
    (&["th", "pearls", "checkpoint"], "a pearl checkpoint"),
    (&["th", "pearls", "close"], "closing a pearl"),
];

/// Test runners: a milestone only when the run passed.
const TEST_RUNS: &[&[&str]] = &[
    &["cargo", "test"],
    &["cargo", "nextest"],
    &["pnpm", "test"],
    &["npm", "test"],
    &["yarn", "test"],
    &["bun", "test"],
    &["go", "test"],
    &["vitest"],
    &["pytest"],
];

/// The milestone a PostToolUse payload marks — a commit, push, attest, PR,
/// pearl checkpoint/close, or a passing test run — as a short label.
#[must_use]
pub fn milestone(payload: &Value) -> Option<&'static str> {
    let cmd = shell_command(payload)?;
    let words: Vec<&str> = cmd
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .map(|w| w.rsplit('/').next().unwrap_or(w))
        .collect();
    let has = |seq: &[&str]| words.windows(seq.len()).any(|w| w == seq);
    if let Some((_, label)) = MILESTONES.iter().find(|(seq, _)| has(seq)) {
        return Some(*label);
    }
    if TEST_RUNS.iter().any(|seq| has(seq)) {
        let resp = payload.get("tool_response");
        let interrupted = resp.and_then(|r| r.get("interrupted")).and_then(Value::as_bool) == Some(true);
        let exit = resp
            .and_then(|r| r.get("exit_code").or_else(|| r.get("exitCode")))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if !interrupted && exit == 0 && !output_failed(&tool_output(payload)) {
            return Some("a passing test run");
        }
    }
    None
}

/// A session's lines: who it is and where each one sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits<'a> {
    pub harness: &'a str,
    pub reset: Reset,
    /// A milestone past this suggests `/th-clear` (0 = never).
    pub suggest: u64,
    /// Past this, one note: the reset is near, checkpoint the pearl.
    pub warn: u64,
    /// The reset itself: auto-compaction ([`Reset::Compact`]) or th-clear.
    pub hard: u64,
}

/// The budget decision for one hook event. Never blocks; at most one note.
///
/// - **Suggest** — a [`milestone`] with context past `suggest` asks the agent
///   to tell the user this is a good point to `/th-clear`; again every
///   [`SUGGEST_STEP`] after that.
/// - **Warn** — once, past `warn`: the reset is near, checkpoint the pearl
///   (compact) or wrap up and th-clear (clear).
/// - **Over** — once, past `hard` (clear mode) or `hard` + 10% (compact mode,
///   where the harness should have compacted at `hard` by itself).
///
/// A context under half of the flagged peak means the session compacted (or
/// was cleared), so the flags re-arm for the next segment.
#[must_use]
pub fn decide_session(limits: &Limits<'_>, event: &str, tokens: u64, state: &BudgetState, milestone: Option<&str>) -> (Verdict, BudgetState) {
    let mut next = if state.flagged() && tokens.saturating_mul(2) < state.peak {
        BudgetState::default()
    } else {
        state.clone()
    };
    let verdict = if matches!(event, "PostToolUse" | "UserPromptSubmit") {
        note(limits, event, tokens, &mut next, milestone)
    } else {
        Verdict::Quiet
    };
    if next.flagged() {
        next.peak = next.peak.max(tokens);
    }
    (verdict, next)
}

fn note(limits: &Limits<'_>, event: &str, tokens: u64, s: &mut BudgetState, milestone: Option<&str>) -> Verdict {
    let Limits {
        harness,
        reset,
        suggest,
        warn,
        hard,
    } = *limits;
    let over_line = match reset {
        Reset::Clear => hard,
        Reset::Compact => hard + hard / 10,
    };
    if hard > 0 && tokens >= over_line && !s.over_noted {
        s.over_noted = true;
        s.warned = true;
        let why = match reset {
            Reset::Clear => format!("Context is at {}, over this session's {} budget (harness.context_budget). ", k(tokens), k(hard)),
            Reset::Compact => format!(
                "Context is at {} and this harness has not auto-compacted at the {} budget — it reads the threshold at launch, so this \
                 session predates `th harness enable {harness}`, or the harness has no auto-compaction. ",
                k(tokens),
                k(hard)
            ),
        };
        return Verdict::Context(format!(
            "{why}At the next clean stopping point, invoke the th-clear skill (it checkpoints the work into a pearl and arms the resume) \
             and give the user the line it prints. Until then, keep working."
        ));
    }
    if warn > 0 && tokens >= warn && !s.warned {
        s.warned = true;
        return Verdict::Context(match reset {
            Reset::Compact => format!(
                "Context is at {}; this session auto-compacts at about {} (harness.context_budget). At the next clean stopping point — \
                 a commit, green tests, the end of this step — checkpoint the pearl you are on: `th pearls checkpoint <id> --note \"…\" --next \"…\"`. \
                 That handoff is re-injected after the compaction, so make `next` concrete. Then keep working; no /clear needed.",
                k(tokens),
                k(hard.max(warn))
            ),
            Reset::Clear => format!(
                "Context is at {}; this session's budget is {} (harness.context_budget). Wrap up the current piece of work — \
                 at a clean stopping point, invoke the th-clear skill to hand off and continue in a fresh context.",
                k(tokens),
                k(hard.max(warn))
            ),
        });
    }
    let due = suggest > 0 && tokens >= suggest && (s.suggested_at == 0 || tokens >= s.suggested_at + SUGGEST_STEP);
    if let (true, "PostToolUse", Some(what)) = (due, event, milestone) {
        s.suggested_at = tokens;
        let later = match reset {
            Reset::Compact if hard > 0 => format!(" Otherwise the session auto-compacts at about {}.", k(hard)),
            _ => String::new(),
        };
        return Verdict::Context(format!(
            "Checkpoint reached ({what}) with context at {} (harness.context_suggest {}). Every later call resends all of it, so this is \
             a good point to start fresh. When you next report to the user, add one line suggesting /th-clear — it hands this work to a \
             pearl and resumes it in a clean context. Don't run it unasked; carry on with the task.{later}",
            k(tokens),
            k(suggest)
        ));
    }
    Verdict::Quiet
}

/// Render a verdict as hook stdout for `event`.
#[must_use]
pub fn verdict_output(v: &Verdict, event: &str, format: &str) -> Option<String> {
    match v {
        Verdict::Quiet => None,
        Verdict::Context(msg) if format == "text" => Some(msg.clone()),
        Verdict::Context(msg) => Some(json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": msg}}).to_string()),
    }
}

/// Context tokens and model from a Claude Code statusline payload:
/// `context_window.current_usage` when present, else the transcript tail.
#[must_use]
pub fn statusline_context(payload: &Value) -> Option<(u64, Option<String>)> {
    let model = payload
        .pointer("/model/id")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    if let Some(u) = payload.pointer("/context_window/current_usage").filter(|u| u.is_object()) {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let total = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
        if total > 0 {
            return Some((total, model));
        }
    }
    let (tokens, seen) = transcript_context(Path::new(pstr(payload, "transcript_path")?))?;
    Some((tokens, model.or(seen)))
}

/// The statusline segment: `◔ 162k/300k 54%`, dim under `suggest`, yellow
/// from `suggest` (a good time to /th-clear), red from `warn`.
#[must_use]
pub fn statusline_segment(tokens: u64, suggest: u64, warn: u64, hard: u64) -> String {
    const DIM: &str = "\x1b[2m";
    const YELLOW: &str = "\x1b[33m";
    const RED: &str = "\x1b[31m";
    const RESET: &str = "\x1b[0m";
    let color = if warn > 0 && tokens >= warn {
        RED
    } else if suggest > 0 && tokens >= suggest {
        YELLOW
    } else {
        DIM
    };
    if hard == 0 {
        return format!("{color}◔ {}{RESET}", k(tokens));
    }
    let pct = tokens.saturating_mul(100) / hard;
    format!("{color}◔ {}/{} {pct}%{RESET}", k(tokens), k(hard))
}

/// `th harness budget --statusline`: read a statusline payload on stdin and
/// print the context segment (nothing when the size is unknown).
pub fn statusline_cmd(harness: &str) {
    let payload = read_payload();
    let Some((tokens, model)) = statusline_context(&payload) else { return };
    let budgets = Budgets::from_settings();
    let (hard, warn) = budgets.for_session(harness, model.as_deref());
    let suggest = budgets.suggest_for(harness, model.as_deref());
    print!("{}", statusline_segment(tokens, suggest, warn, hard));
}
fn safe_id(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(80).collect()
}

/// `th harness budget`: the hook entry. Silent unless a note is due; never fails, never blocks.
pub fn budget_cmd(harness: &str, event: Option<String>, tokens: Option<u64>, model: Option<String>, format: Option<String>) {
    let payload = read_payload();
    let budgets = Budgets::from_settings();
    let event = event.or_else(|| pstr(&payload, "hook_event_name").map(str::to_string));
    let seen = pstr(&payload, "transcript_path").and_then(|p| transcript_context(Path::new(p)));
    let tokens = tokens.or_else(|| seen.as_ref().map(|(n, _)| *n));
    let model = model.or_else(|| seen.and_then(|(_, m)| m));
    let (hard, warn) = budgets.for_session(harness, model.as_deref());
    let suggest = budgets.suggest_for(harness, model.as_deref());
    let Some(event) = event else {
        let mode = match budgets.reset {
            Reset::Compact => "auto-compact",
            Reset::Clear => "/th-clear",
        };
        let off = |n: u64| if n == 0 { "off".into() } else { k(n) };
        println!(
            "suggest /th-clear {} · warn {} · {mode} {} · {harness}{} · current {}",
            off(suggest),
            off(warn),
            off(hard),
            model.as_deref().map(|m| format!(" / {m}")).unwrap_or_default(),
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
    let limits = Limits {
        harness,
        reset: budgets.reset,
        suggest,
        warn,
        hard,
    };
    let (verdict, next) = decide_session(&limits, &event, tokens, &state, milestone(&payload));
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

    fn resumed(c: Claim) -> Armed {
        match c {
            Claim::Resumed(t) => t,
            other => panic!("expected Resumed, got {other:?}"),
        }
    }

    #[test]
    fn arm_then_claim_is_one_shot() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), 5, now)).unwrap();
        let got = resumed(claim_in(dir.path(), "/w", &[10], Source::Clear, now, |_| false));
        assert_eq!(got.pearls, vec!["th-abc123".to_string()]);
        assert_eq!(claim_in(dir.path(), "/w", &[10], Source::Clear, now, |_| false), Claim::Nothing);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "claimed token is gone");
    }

    /// SMOODEV-3888 regression: armed with a 15m TTL, `/clear` typed 52m later
    /// in the SAME terminal must still resume (it used to be reaped silently).
    #[test]
    fn same_terminal_claims_past_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let armed_at = Utc::now();
        arm_in(dir.path(), &token("/w", Some(44434), 15, armed_at)).unwrap();
        let later = armed_at + Duration::minutes(52);
        let got = resumed(claim_in(dir.path(), "/w", &[44434], Source::Clear, later, |_| true));
        assert_eq!(got.pearls, vec!["th-abc123".to_string()]);
    }

    #[test]
    fn same_terminal_grace_has_a_limit() {
        let dir = tempfile::tempdir().unwrap();
        let armed_at = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), 15, armed_at)).unwrap();
        let much_later = armed_at + Duration::minutes(15) + Duration::hours(SAME_TERMINAL_GRACE_HOURS);
        assert_eq!(claim_in(dir.path(), "/w", &[10], Source::Clear, much_later, |_| true), Claim::Nothing);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "past the grace the token is reaped");
    }

    #[test]
    fn expired_foreign_token_is_not_claimed_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), -1, now)).unwrap();
        match claim_in(dir.path(), "/w", &[99], Source::Startup, now, |_| true) {
            Claim::Missed(m) => {
                assert_eq!(m.len(), 1);
                assert!(m[0].expired);
            }
            other => panic!("expected Missed, got {other:?}"),
        }
        assert_eq!(tokens_in(dir.path()).len(), 1, "owner still alive: keep it for its own /clear");
    }

    #[test]
    fn expired_token_of_a_dead_terminal_is_reported_once_then_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), -1, now)).unwrap();
        assert!(matches!(claim_in(dir.path(), "/w", &[99], Source::Startup, now, |_| false), Claim::Missed(_)));
        assert_eq!(claim_in(dir.path(), "/w", &[99], Source::Startup, now, |_| false), Claim::Nothing);
    }

    #[test]
    fn another_terminals_fresh_token_is_reported_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), 5, now)).unwrap();
        match claim_in(dir.path(), "/w", &[99], Source::Clear, now, |_| false) {
            Claim::Missed(m) => assert!(!m[0].expired),
            other => panic!("expected Missed, got {other:?}"),
        }
        assert_eq!(tokens_in(dir.path()).len(), 1, "a fresh token stays for its own terminal");
    }

    #[test]
    fn compact_or_resume_never_claims_or_notes() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/w", Some(10), 5, now)).unwrap();
        assert_eq!(claim_in(dir.path(), "/w", &[10], Source::Other, now, |_| true), Claim::Nothing);
        assert_eq!(tokens_in(dir.path()).len(), 1);
    }

    #[test]
    fn claim_ignores_other_cwds() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        arm_in(dir.path(), &token("/other", Some(10), 5, now)).unwrap();
        assert_eq!(claim_in(dir.path(), "/w", &[10], Source::Clear, now, |_| true), Claim::Nothing);
        assert_eq!(tokens_in(dir.path()).len(), 1);
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
        let got = resumed(claim_in(dir.path(), "/w", &[10], Source::Clear, now, |_| false));
        assert_eq!(got.pearls, vec!["th-def456".to_string()]);
    }

    #[test]
    fn claim_with_missing_dir_is_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert_eq!(claim_in(&missing, "/w", &[], Source::Clear, Utc::now(), |_| false), Claim::Nothing);
    }

    #[test]
    fn missed_context_names_pearls_age_and_reason() {
        let now = Utc::now();
        let mut old = token("/w", Some(10), 15, now - Duration::minutes(52));
        old.pearls = vec!["th-c6dc33".into()];
        let ctx = missed_context(
            &[
                Missed { token: old, expired: true },
                Missed {
                    token: token("/w", Some(20), 5, now),
                    expired: false,
                },
            ],
            now,
        );
        assert!(ctx.contains("th-c6dc33 (armed 52m ago, expired)"), "{ctx}");
        assert!(ctx.contains("th-abc123 (armed 0m ago, armed by another terminal)"), "{ctx}");
        assert!(ctx.contains("th pearls show <id> --handoff"));
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
        assert_eq!(usage_context(l).map(|(n, _)| n), Some(3210));
        let side = r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":10}}}"#;
        assert_eq!(usage_context(side).map(|(n, _)| n), None);
        assert_eq!(usage_context(r#"{"type":"user","message":{}}"#).map(|(n, _)| n), None);
        assert_eq!(usage_context("not json").map(|(n, _)| n), None);
        assert_eq!(usage_context(r#"{"type":"assistant","message":{"usage":{}}}"#).map(|(n, _)| n), None);
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
        assert_eq!(transcript_context(&p).map(|(n, _)| n), Some(5002));
        assert_eq!(transcript_context(&dir.path().join("missing.jsonl")), None);
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
        assert_eq!(transcript_context(&p).map(|(n, _)| n), Some(240_004));
    }

    #[test]
    fn resume_commands_are_bare_lines_for_the_clipboard() {
        assert_eq!(resume_commands("claude-code", &["th-a".into()]), "/th-handoff resume th-a");
        assert_eq!(
            resume_commands("codex", &["a".into(), "b".into()]),
            "$th-handoff resume a\n$th-handoff resume b"
        );
    }

    #[test]
    fn resume_instructions_name_clear_and_resume_per_harness() {
        let p = vec!["th-abc123".to_string()];
        let claude = resume_instructions("claude-code", &p);
        assert!(claude.contains("/clear") && claude.contains("/th-handoff resume th-abc123"), "{claude}");
        let codex = resume_instructions("codex", &p);
        assert!(codex.contains("/new") && codex.contains("$th-handoff resume th-abc123"), "{codex}");
        let two = resume_instructions("claude-code", &["a".to_string(), "b".to_string()]);
        assert!(two.contains("/th-handoff resume a") && two.contains("/th-handoff resume b"), "{two}");
    }

    fn limits(reset: Reset) -> Limits<'static> {
        Limits {
            harness: "claude-code",
            reset,
            suggest: 150_000,
            warn: 270_000,
            hard: 300_000,
        }
    }

    fn session(event: &str, tokens: u64, state: &BudgetState) -> (Verdict, BudgetState) {
        decide_session(&limits(Reset::Compact), event, tokens, state, None)
    }

    fn at_milestone(tokens: u64, state: &BudgetState) -> (Verdict, BudgetState) {
        decide_session(&limits(Reset::Compact), "PostToolUse", tokens, state, Some("a commit"))
    }

    fn bash(cmd: &str, response: &Value) -> Value {
        json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": {"command": cmd}, "tool_response": response})
    }

    #[test]
    fn quiet_under_every_line() {
        let (v, s) = at_milestone(100_000, &BudgetState::default());
        assert_eq!(v, Verdict::Quiet);
        assert_eq!(s, BudgetState::default(), "no flag → no peak → no file write");
    }

    #[test]
    fn nothing_ever_blocks() {
        for reset in [Reset::Compact, Reset::Clear] {
            for tokens in [100_000, 280_000, 320_000, 900_000] {
                let (v, s) = decide_session(&limits(reset), "Stop", tokens, &BudgetState::default(), Some("a commit"));
                assert_eq!(v, Verdict::Quiet, "{reset:?} {tokens}: Stop says nothing");
                assert_eq!(s, BudgetState::default());
            }
        }
    }

    #[test]
    fn a_milestone_past_suggest_suggests_th_clear_then_every_50k() {
        let (v, s) = at_milestone(160_000, &BudgetState::default());
        let Verdict::Context(m) = v else { panic!("expected a suggestion") };
        assert!(
            m.contains("a commit") && m.contains("160k") && m.contains("/th-clear") && m.contains("unasked"),
            "{m}"
        );
        assert!(m.contains("auto-compacts at about 300k"), "{m}");
        assert_eq!(s.suggested_at, 160_000);
        assert_eq!(at_milestone(200_000, &s).0, Verdict::Quiet, "under +50k");
        let (v, s2) = at_milestone(210_000, &s);
        assert!(matches!(v, Verdict::Context(ref m) if m.contains("210k")), "+50k suggests again");
        assert_eq!(s2.suggested_at, 210_000);
    }

    #[test]
    fn suggestions_need_a_milestone_on_post_tool_use() {
        assert_eq!(session("PostToolUse", 200_000, &BudgetState::default()).0, Verdict::Quiet, "no milestone");
        let (v, _) = decide_session(&limits(Reset::Compact), "UserPromptSubmit", 200_000, &BudgetState::default(), Some("a commit"));
        assert_eq!(v, Verdict::Quiet, "a prompt is not a milestone");
        let mut off = limits(Reset::Compact);
        off.suggest = 0;
        assert_eq!(
            decide_session(&off, "PostToolUse", 200_000, &BudgetState::default(), Some("a push")).0,
            Verdict::Quiet
        );
    }

    #[test]
    fn compact_mode_warns_once_with_a_checkpoint_not_a_clear() {
        let (v, s) = session("PostToolUse", 275_000, &BudgetState::default());
        let Verdict::Context(m) = v else { panic!("expected a nudge") };
        assert!(
            m.contains("th pearls checkpoint") && m.contains("auto-compacts") && m.contains("no /clear"),
            "{m}"
        );
        assert!(!m.contains("th-clear skill"), "{m}");
        assert!(s.warned && s.peak == 275_000);
        assert_eq!(session("PostToolUse", 280_000, &s).0, Verdict::Quiet, "once");
        // A milestone after the warn still suggests.
        assert!(matches!(at_milestone(280_000, &s).0, Verdict::Context(ref m) if m.contains("/th-clear")));
    }

    #[test]
    fn compact_mode_notes_th_clear_only_past_budget_plus_ten_percent() {
        let warned = session("PostToolUse", 275_000, &BudgetState::default()).1;
        assert_eq!(
            session("PostToolUse", 320_000, &warned).0,
            Verdict::Quiet,
            "inside the slack the harness compacts"
        );
        let (v, s) = session("PostToolUse", 335_000, &warned);
        let Verdict::Context(m) = v else {
            panic!("expected a note past budget + 10%")
        };
        assert!(
            m.contains("has not auto-compacted") && m.contains("th harness enable claude-code") && m.contains("th-clear"),
            "{m}"
        );
        assert!(s.over_noted);
        assert_eq!(session("PostToolUse", 345_000, &s).0, Verdict::Quiet, "once");
    }

    #[test]
    fn clear_mode_nudges_to_th_clear_without_blocking() {
        let clear = limits(Reset::Clear);
        let (v, s) = decide_session(&clear, "PostToolUse", 275_000, &BudgetState::default(), None);
        assert!(matches!(v, Verdict::Context(ref m) if m.contains("th-clear skill") && m.contains("300k")));
        let (v, s) = decide_session(&clear, "UserPromptSubmit", 301_000, &s, None);
        assert!(matches!(v, Verdict::Context(ref m) if m.contains("over this session's 300k") && !m.contains("has not auto-compacted")));
        assert!(s.over_noted);
        assert_eq!(decide_session(&clear, "Stop", 310_000, &s, None).0, Verdict::Quiet);
    }

    #[test]
    fn a_compaction_re_arms_the_nudges() {
        let (_, warned) = session("PostToolUse", 280_000, &BudgetState::default());
        let (_, warned) = at_milestone(285_000, &warned);
        assert!(warned.warned && warned.suggested_at == 285_000);
        // Compacted to 60k: under half the peak → fresh flags, and nothing to say yet.
        let (v, s) = session("PostToolUse", 60_000, &warned);
        assert_eq!(v, Verdict::Quiet);
        assert_eq!(s, BudgetState::default());
        // Back past the suggest line, a milestone suggests again straight away.
        assert!(matches!(at_milestone(155_000, &s).0, Verdict::Context(_)));
        // A dip that is not a compaction keeps the flags.
        let (_, kept) = session("PostToolUse", 200_000, &warned);
        assert!(kept.warned);
    }

    #[test]
    fn verdict_output_shapes() {
        let c: Value = serde_json::from_str(&verdict_output(&Verdict::Context("m".into()), "PostToolUse", "json").unwrap()).unwrap();
        assert_eq!(c["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(c["hookSpecificOutput"]["additionalContext"], "m");
        assert_eq!(verdict_output(&Verdict::Context("m".into()), "PostToolUse", "text").as_deref(), Some("m"));
        assert!(verdict_output(&Verdict::Quiet, "Stop", "json").is_none());
    }

    #[test]
    fn old_state_files_still_load() {
        let s: BudgetState = serde_json::from_str(r#"{"warned":true,"over_noted":false,"blocked":true,"peak":5}"#).unwrap();
        assert!(s.warned && s.suggested_at == 0 && s.peak == 5);
    }

    #[test]
    fn milestones_are_work_boundaries() {
        let ok = json!({"stdout": "done", "stderr": "", "interrupted": false});
        let commit = ["git", "commit -m x"].join(" ");
        assert_eq!(milestone(&bash(&commit, &ok)), Some("a commit"));
        assert_eq!(milestone(&bash("cd /x && git -C . status; git push -u origin b", &ok)), Some("a push"));
        assert_eq!(milestone(&bash("th attest --all", &ok)), Some("th attest"));
        assert_eq!(milestone(&bash("gh pr create --title t", &ok)), Some("opening a PR"));
        assert_eq!(milestone(&bash("gh pr merge 5 --squash", &ok)), Some("a merge"));
        assert_eq!(milestone(&bash("th pearls checkpoint th-1 --note n", &ok)), Some("a pearl checkpoint"));
        assert_eq!(milestone(&bash("/usr/local/bin/th pearls close th-1", &ok)), Some("closing a pearl"));
        assert_eq!(milestone(&bash("git status", &ok)), None);
        assert_eq!(milestone(&bash("echo th-attest", &ok)), None);
        assert_eq!(milestone(&json!({"tool_name": "Edit", "tool_input": {"file_path": "x"}})), None);
        // Codex passes the command as argv.
        let argv = json!({"tool_name": "shell", "tool_input": {"command": ["bash", "-lc", "git push"]}});
        assert_eq!(milestone(&argv), Some("a push"));
    }

    #[test]
    fn only_a_passing_test_run_is_a_milestone() {
        let pass = json!({"stdout": "test result: ok. 102 passed; 0 failed; 0 ignored", "stderr": ""});
        assert_eq!(milestone(&bash("cargo test -p x", &pass)), Some("a passing test run"));
        let fail = json!({"stdout": "test result: FAILED. 1 passed; 1 failed", "stderr": ""});
        assert_eq!(milestone(&bash("cargo test", &fail)), None);
        let vitest = json!({"stdout": "Tests  2 failed | 5 passed", "stderr": ""});
        assert_eq!(milestone(&bash("pnpm test", &vitest)), None);
        let code = json!({"stdout": "", "stderr": "", "exit_code": 1});
        assert_eq!(milestone(&bash("pytest", &code)), None);
        let cut = json!({"stdout": "", "stderr": "", "interrupted": true});
        assert_eq!(milestone(&bash("go test ./...", &cut)), None);
        assert_eq!(
            milestone(&bash("th ci-queue run --lock cargo -- cargo test", &json!("ok"))),
            Some("a passing test run")
        );
    }

    #[test]
    fn output_failed_reads_counts() {
        assert!(!output_failed("3 passed; 0 failed"));
        assert!(output_failed("1 failed, 2 passed"));
        assert!(output_failed("  10 failed"));
        assert!(!output_failed("nothing failed here"));
        assert!(output_failed("FAIL src/a.test.ts"));
    }

    #[test]
    fn statusline_reads_the_payload_then_the_transcript() {
        let p = json!({"model": {"id": "claude-opus-5-5"}, "context_window": {"current_usage": {"input_tokens": 10, "cache_read_input_tokens": 161_990}}});
        assert_eq!(statusline_context(&p), Some((162_000, Some("claude-opus-5-5".into()))));
        let dir = tempfile::TempDir::new().unwrap();
        let t = dir.path().join("t.jsonl");
        fs::write(&t, r#"{"type":"assistant","message":{"model":"claude-sonnet-5-5","usage":{"input_tokens":7}}}"#).unwrap();
        let p = json!({"transcript_path": t.to_str().unwrap(), "context_window": {"current_usage": null}});
        assert_eq!(statusline_context(&p), Some((7, Some("claude-sonnet-5-5".into()))));
        assert_eq!(statusline_context(&json!({})), None);
    }

    #[test]
    fn statusline_segment_colors_by_line() {
        let dim = statusline_segment(100_000, 150_000, 270_000, 300_000);
        assert!(dim.contains("\x1b[2m") && dim.contains("100k/300k 33%"), "{dim:?}");
        assert!(statusline_segment(160_000, 150_000, 270_000, 300_000).contains("\x1b[33m"));
        assert!(statusline_segment(280_000, 150_000, 270_000, 300_000).contains("\x1b[31m"));
        let off = statusline_segment(50_000, 0, 0, 0);
        assert!(off.contains("◔ 50k") && !off.contains('%'), "{off:?}");
    }

    #[test]
    fn budget_settings_fall_back_to_registry_defaults() {
        use smooth_policy::settings::{Resolver, SettingsFile};
        let none = Budgets::from_resolver(&Resolver::new(|_| None, SettingsFile::empty()));
        assert_eq!((none.global, none.warn, none.suggest, none.reset), (300_000, 270_000, 150_000, Reset::Compact));
        let env = Budgets::from_resolver(&Resolver::new(
            |k| match k {
                "SMOOTH_CONTEXT_BUDGET" | "SMOOTH_CONTEXT_SUGGEST" => Some("0".into()),
                "SMOOTH_CONTEXT_BUDGET_WARN" => Some("-5".into()),
                "SMOOTH_CONTEXT_RESET" => Some("clear".into()),
                _ => None,
            },
            SettingsFile::empty(),
        ));
        assert_eq!(env.global, 0, "0 disables");
        assert_eq!(env.warn, 0, "negative is disabled");
        assert_eq!(env.suggest, 0);
        assert_eq!(env.reset, Reset::Clear);
    }

    #[test]
    fn usage_context_carries_the_model() {
        let l = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","usage":{"input_tokens":10,"cache_read_input_tokens":90}}}"#;
        assert_eq!(usage_context(l), Some((100, Some("claude-opus-5-5".into()))));
        let synthetic = r#"{"type":"assistant","message":{"model":"<synthetic>","usage":{"input_tokens":5}}}"#;
        assert_eq!(usage_context(synthetic), Some((5, None)));
        let none = r#"{"type":"assistant","message":{"usage":{"input_tokens":5}}}"#;
        assert_eq!(usage_context(none), Some((5, None)));
    }

    #[test]
    fn safe_id_strips_path_chars() {
        assert_eq!(safe_id("../../etc/passwd"), "etcpasswd");
        assert_eq!(safe_id("abc-123_X"), "abc-123_X");
    }
}
