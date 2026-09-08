//! `th pearls checkpoint` / `show --handoff` / `prime --in-progress` — the
//! compaction-proof handoff packet (pearl th-9483e8, SmoothFlow lane C).
//!
//! Storage: every checkpoint is ONE pearl comment whose content is
//! [`CHECKPOINT_PREFIX`] followed by a JSON [`Checkpoint`]. Comments are
//! append-only with a server timestamp, which is exactly a checkpoint log,
//! and they ride on the public `PearlStore` API alone — no schema change, so
//! this survives the Dolt→SQLite store swap (th-d3e842) untouched. The
//! effective handoff is the field-wise merge of every checkpoint in order
//! (latest non-null wins); notes accumulate.
//!
//! ponytail: comment-as-record, not a table. Move to a real column set only if
//! a pearl accrues hundreds of auto-checkpoints and `show` gets slow.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use smooth_pearls::{Pearl, PearlComment, PearlStatus, PearlStore};
use std::path::Path;
use std::process::Command;

/// Comment prefix that marks a checkpoint record. Anything else is a human comment.
pub const CHECKPOINT_PREFIX: &str = "smooth-checkpoint:";
/// `git status --porcelain` rows kept per checkpoint (a handoff, not a diff).
pub const DIRTY_CAP: usize = 50;

/// The "where is the work" half of a checkpoint. Every field optional so a
/// checkpoint taken outside a git repo still records the note.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

impl Handoff {
    /// Field-wise overlay: `later`'s `Some` fields win.
    fn overlay(&mut self, later: &Self) {
        macro_rules! take {
            ($($f:ident),*) => { $( if later.$f.is_some() { self.$f.clone_from(&later.$f); } )* };
        }
        take!(worktree, branch, head, dirty, agent_session_id, next);
    }
}

/// One checkpoint record, as stored in the comment JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub handoff: Handoff,
}

/// What `th pearls checkpoint` was asked to record.
#[derive(Debug, Clone, Default)]
pub struct CheckpointOpts {
    pub note: Option<String>,
    pub next: Option<String>,
    pub auto: bool,
    pub session_id: Option<String>,
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Collect worktree / branch / HEAD / dirty list from `cwd`. All `None` when
/// `cwd` isn't inside a git repo.
#[must_use]
pub fn collect_git_state(cwd: &Path) -> Handoff {
    let Some(worktree) = git(cwd, &["rev-parse", "--show-toplevel"]) else {
        return Handoff::default();
    };
    let dirty = git(cwd, &["status", "--porcelain"]).map(|s| s.lines().map(|l| l.trim_start().to_string()).take(DIRTY_CAP).collect());
    Handoff {
        worktree: Some(worktree),
        branch: git(cwd, &["branch", "--show-current"]).filter(|b| !b.is_empty()),
        head: git(cwd, &["rev-parse", "HEAD"]),
        dirty,
        ..Default::default()
    }
}

/// Build the checkpoint record for `cwd` + `opts` (pure over its inputs apart
/// from the git probes and the clock).
#[must_use]
pub fn build_checkpoint(cwd: &Path, opts: &CheckpointOpts) -> Checkpoint {
    let mut handoff = collect_git_state(cwd);
    handoff.agent_session_id = opts
        .session_id
        .clone()
        .or_else(|| std::env::var("CLAUDE_SESSION_ID").ok())
        .filter(|s| !s.is_empty());
    handoff.next.clone_from(&opts.next);
    Checkpoint {
        at: Utc::now(),
        note: opts.note.clone().filter(|n| !n.trim().is_empty()),
        auto: opts.auto,
        handoff,
    }
}

/// Serialize a checkpoint into its comment form.
///
/// # Errors
/// Only if serde_json fails, which it can't for these plain structs.
pub fn encode(cp: &Checkpoint) -> Result<String> {
    Ok(format!("{CHECKPOINT_PREFIX}{}", serde_json::to_string(cp)?))
}

/// Parse a comment back into a checkpoint; `None` for human comments or a
/// record this build can't read (never fail the whole packet over one row).
#[must_use]
pub fn decode(content: &str) -> Option<Checkpoint> {
    serde_json::from_str(content.strip_prefix(CHECKPOINT_PREFIX)?.trim()).ok()
}

#[must_use]
pub fn is_checkpoint_comment(c: &PearlComment) -> bool {
    c.content.starts_with(CHECKPOINT_PREFIX)
}

/// Checkpoints among `comments`, oldest first (the store already orders them).
#[must_use]
pub fn parse_checkpoints(comments: &[PearlComment]) -> Vec<Checkpoint> {
    comments.iter().filter_map(|c| decode(&c.content)).collect()
}

/// The effective handoff: every checkpoint overlaid in order.
#[must_use]
pub fn merged_handoff(checkpoints: &[Checkpoint]) -> Handoff {
    let mut h = Handoff::default();
    for cp in checkpoints {
        h.overlay(&cp.handoff);
    }
    h
}

/// Record a checkpoint on `id`. Returns the stored record.
///
/// # Errors
/// Unknown pearl or a store write failure.
pub fn checkpoint(store: &PearlStore, id: &str, cwd: &Path, opts: &CheckpointOpts) -> Result<Checkpoint> {
    if store.get(id)?.is_none() {
        anyhow::bail!("issue not found: {id}");
    }
    let cp = build_checkpoint(cwd, opts);
    store.add_comment(id, &encode(&cp)?)?;
    Ok(cp)
}

/// PR summary for the handoff branch via `gh` — `None` when gh is missing,
/// unauthenticated, offline, or there is no PR. `ci` folds
/// `statusCheckRollup` to `success` / `failure` / `pending` / `null`.
#[must_use]
pub fn pr_for_branch(branch: &str, cwd: &Path) -> Option<serde_json::Value> {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--limit",
            "1",
            "--json",
            "number,url,state,statusCheckRollup",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let list: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    let pr = list.into_iter().next()?;
    Some(serde_json::json!({
        "number": pr.get("number"),
        "url": pr.get("url"),
        "state": pr.get("state"),
        "ci": summarize_ci(pr.get("statusCheckRollup")),
    }))
}

/// Fold gh's `statusCheckRollup` array into one word. Exposed for tests.
#[must_use]
pub fn summarize_ci(rollup: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(items) = rollup.and_then(|r| r.as_array()) else {
        return serde_json::Value::Null;
    };
    if items.is_empty() {
        return serde_json::Value::Null;
    }
    let mut pending = false;
    for it in items {
        // CheckRun rows carry `conclusion` (+ `status`), StatusContext rows carry `state`.
        let word = it
            .get("conclusion")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| it.get("state").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_ascii_uppercase();
        match word.as_str() {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => return "failure".into(),
            _ => pending = true,
        }
    }
    if pending {
        "pending".into()
    } else {
        "success".into()
    }
}

/// The handoff packet for one pearl — the SmoothFlow "Pearl rail" shape:
/// `{pearl, handoff, checkpoints:[{at,note,auto}], blocks:[ids], pr|null}`.
/// `blocks` lists the OPEN pearls this one still waits on (what `th pearls
/// blocked` would show); `pr` is looked up only when `with_pr` is set — it
/// shells out to `gh`, so hot paths (PreCompact) skip it.
///
/// # Errors
/// Store read failures.
pub fn packet(store: &PearlStore, pearl: &Pearl, with_pr: bool) -> Result<serde_json::Value> {
    let cps = parse_checkpoints(&store.get_comments(&pearl.id)?);
    let handoff = merged_handoff(&cps);
    let blocks: Vec<String> = store
        .get_blockers(&pearl.id)?
        .into_iter()
        .filter(|b| b.status != PearlStatus::Closed)
        .map(|b| b.id)
        .collect();
    let pr = if with_pr {
        handoff.branch.as_deref().and_then(|b| {
            let cwd = handoff
                .worktree
                .as_deref()
                .map_or_else(|| Path::new(".").to_path_buf(), std::path::PathBuf::from);
            let cwd = if cwd.is_dir() { cwd } else { Path::new(".").to_path_buf() };
            pr_for_branch(b, &cwd)
        })
    } else {
        None
    };
    let checkpoints: Vec<serde_json::Value> = cps.iter().map(|c| serde_json::json!({"at": c.at, "note": c.note, "auto": c.auto})).collect();
    Ok(serde_json::json!({
        "pearl": pearl,
        "handoff": handoff,
        "checkpoints": checkpoints,
        "blocks": blocks,
        "pr": pr,
    }))
}

/// Does this pearl belong to the session running in `cwd`? True when its
/// recorded worktree is `cwd`'s toplevel, or its id appears in the current
/// branch name (`th-9483e8-handoff`) — the convention `th worktree create`
/// follows, so a pearl claimed but never checkpointed still matches.
#[must_use]
pub fn matches_cwd(pearl_id: &str, handoff: &Handoff, cwd_toplevel: Option<&str>, cwd_branch: Option<&str>) -> bool {
    let by_worktree = matches!((handoff.worktree.as_deref(), cwd_toplevel), (Some(w), Some(t)) if w == t);
    let by_branch = cwd_branch.is_some_and(|b| b.contains(pearl_id));
    by_worktree || by_branch
}

/// In-progress pearls, optionally narrowed to an assignee and/or to the
/// session in `cwd` (see [`matches_cwd`]).
///
/// # Errors
/// Store read failures.
pub fn in_progress(store: &PearlStore, assignee: Option<&str>, cwd: Option<&Path>) -> Result<Vec<Pearl>> {
    let mut pearls = store.list(&smooth_pearls::PearlQuery::new().with_status(PearlStatus::InProgress))?;
    if let Some(a) = assignee {
        pearls.retain(|p| p.assigned_to.as_deref() == Some(a));
    }
    if let Some(cwd) = cwd {
        let top = git(cwd, &["rev-parse", "--show-toplevel"]);
        let branch = git(cwd, &["branch", "--show-current"]);
        let mut kept = Vec::new();
        for p in pearls {
            let h = merged_handoff(&parse_checkpoints(&store.get_comments(&p.id)?));
            if matches_cwd(&p.id, &h, top.as_deref(), branch.as_deref()) {
                kept.push(p);
            }
        }
        pearls = kept;
    }
    Ok(pearls)
}

/// The compact "resume cold" block: what it is, where it is, what happened,
/// what is next. Plain text — it is injected into an agent's context.
#[must_use]
pub fn render(packet: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let pearl = &packet["pearl"];
    let h = &packet["handoff"];
    let mut out = String::new();
    let _ = writeln!(out, "## {} — {}", s(pearl, "id").unwrap_or_default(), s(pearl, "title").unwrap_or_default());
    let _ = writeln!(out, "status: {}", s(pearl, "status").unwrap_or_default());
    if let Some(d) = s(pearl, "description").filter(|d| !d.is_empty()) {
        let _ = writeln!(out, "{d}");
    }
    let _ = writeln!(out, "\nwhere:");
    let _ = writeln!(
        out,
        "  worktree: {}",
        s(h, "worktree").unwrap_or_else(|| "(none recorded — checkpoint from the worktree)".into())
    );
    let _ = writeln!(out, "  branch:   {}", s(h, "branch").unwrap_or_else(|| "-".into()));
    let _ = writeln!(
        out,
        "  head:     {}",
        s(h, "head").map_or_else(|| "-".into(), |x| x.chars().take(12).collect::<String>())
    );
    if let Some(dirty) = h.get("dirty").and_then(|d| d.as_array()) {
        if dirty.is_empty() {
            let _ = writeln!(out, "  dirty:    clean");
        } else {
            let _ = writeln!(out, "  dirty:    {} file(s)", dirty.len());
            for f in dirty.iter().take(10) {
                let _ = writeln!(out, "    {}", f.as_str().unwrap_or(""));
            }
            if dirty.len() > 10 {
                let _ = writeln!(out, "    … +{}", dirty.len() - 10);
            }
        }
    }
    if let Some(sid) = s(h, "agent_session_id") {
        let _ = writeln!(out, "  session:  {sid}");
    }
    match packet.get("pr") {
        Some(pr) if !pr.is_null() => {
            let _ = writeln!(
                out,
                "  pr:       #{} {} (ci: {})",
                pr["number"],
                pr["url"].as_str().unwrap_or(""),
                pr["ci"].as_str().unwrap_or("unknown")
            );
        }
        _ => {}
    }
    if let Some(blocks) = packet.get("blocks").and_then(|b| b.as_array()).filter(|b| !b.is_empty()) {
        let ids: Vec<&str> = blocks.iter().filter_map(|b| b.as_str()).collect();
        let _ = writeln!(out, "  blocked by: {}", ids.join(", "));
    }
    let cps = packet.get("checkpoints").and_then(|c| c.as_array()).cloned().unwrap_or_default();
    let notes: Vec<&serde_json::Value> = cps.iter().filter(|c| c["note"].as_str().is_some_and(|n| !n.is_empty())).collect();
    let _ = writeln!(out, "\nwhat happened ({} checkpoint(s), {} with notes):", cps.len(), notes.len());
    for c in notes.iter().rev().take(8).rev() {
        let at = c["at"].as_str().map(|a| a.chars().take(16).collect::<String>()).unwrap_or_default();
        let _ = writeln!(out, "  {} {}", at, c["note"].as_str().unwrap_or(""));
    }
    if let Some(last) = cps.last() {
        let _ = writeln!(
            out,
            "  last checkpoint: {}{}",
            last["at"].as_str().unwrap_or(""),
            if last["auto"].as_bool() == Some(true) { " (auto)" } else { "" }
        );
    }
    let _ = writeln!(
        out,
        "\nnext: {}",
        s(h, "next").unwrap_or_else(|| "(not recorded — set with `th pearls checkpoint <id> --next \"…\"`)".into())
    );
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;

    fn comment(content: &str) -> PearlComment {
        PearlComment {
            id: "c".into(),
            pearl_id: "th-000000".into(),
            content: content.into(),
            created_at: Utc::now(),
        }
    }

    fn cp(note: Option<&str>, auto: bool, handoff: Handoff) -> Checkpoint {
        Checkpoint {
            at: Utc::now(),
            note: note.map(str::to_string),
            auto,
            handoff,
        }
    }

    #[test]
    fn encode_decode_round_trip() {
        let c = cp(
            Some("did a thing"),
            false,
            Handoff {
                worktree: Some("/w".into()),
                branch: Some("b".into()),
                head: Some("abc".into()),
                dirty: Some(vec!["M x.rs".into()]),
                agent_session_id: Some("sid".into()),
                next: Some("more".into()),
            },
        );
        let enc = encode(&c).unwrap();
        assert!(enc.starts_with(CHECKPOINT_PREFIX));
        assert_eq!(decode(&enc).unwrap(), c);
    }

    #[test]
    fn human_comments_and_garbage_are_not_checkpoints() {
        assert!(decode("just a note").is_none());
        assert!(decode(&format!("{CHECKPOINT_PREFIX}not json")).is_none());
        let comments = vec![
            comment("hello"),
            comment(&encode(&cp(Some("n"), true, Handoff::default())).unwrap()),
            comment("bye"),
        ];
        assert_eq!(parse_checkpoints(&comments).len(), 1);
        assert!(!is_checkpoint_comment(&comments[0]));
        assert!(is_checkpoint_comment(&comments[1]));
    }

    #[test]
    fn merge_latest_wins_but_keeps_earlier_fields() {
        let first = cp(
            Some("start"),
            false,
            Handoff {
                worktree: Some("/w".into()),
                branch: Some("b".into()),
                head: Some("111".into()),
                next: Some("write tests".into()),
                ..Default::default()
            },
        );
        // An auto checkpoint refreshes git state but carries no `next`.
        let second = cp(
            None,
            true,
            Handoff {
                worktree: Some("/w".into()),
                branch: Some("b".into()),
                head: Some("222".into()),
                dirty: Some(vec![]),
                ..Default::default()
            },
        );
        let h = merged_handoff(&[first, second]);
        assert_eq!(h.head.as_deref(), Some("222"));
        assert_eq!(h.next.as_deref(), Some("write tests"));
        assert_eq!(h.dirty, Some(vec![]));
    }

    #[test]
    fn merge_of_nothing_is_empty() {
        assert_eq!(merged_handoff(&[]), Handoff::default());
    }

    #[test]
    fn git_state_outside_a_repo_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // A tempdir can sit inside a repo on some CI boxes; guard by checking git's own answer.
        if git(tmp.path(), &["rev-parse", "--show-toplevel"]).is_some() {
            return;
        }
        assert_eq!(collect_git_state(tmp.path()), Handoff::default());
    }

    #[test]
    fn git_state_inside_a_repo_reports_branch_head_and_dirty() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let run = |args: &[&str]| {
            let st = Command::new("git").arg("-C").arg(repo).args(args).status().unwrap();
            assert!(st.success(), "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t.t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(repo.join("b.txt"), "b").unwrap();
        let h = collect_git_state(repo);
        assert_eq!(h.branch.as_deref(), Some("main"));
        assert_eq!(h.head.as_ref().map(String::len), Some(40));
        assert_eq!(h.dirty, Some(vec!["?? b.txt".to_string()]));
        assert!(h.worktree.is_some());
    }

    #[test]
    fn build_checkpoint_trims_empty_note_and_takes_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let c = build_checkpoint(
            tmp.path(),
            &CheckpointOpts {
                note: Some("   ".into()),
                next: Some("n".into()),
                auto: true,
                session_id: Some("sid-1".into()),
            },
        );
        assert!(c.auto);
        assert_eq!(c.note, None);
        assert_eq!(c.handoff.next.as_deref(), Some("n"));
        assert_eq!(c.handoff.agent_session_id.as_deref(), Some("sid-1"));
    }

    #[test]
    fn matches_cwd_by_worktree_or_branch() {
        let h = Handoff {
            worktree: Some("/w/x".into()),
            ..Default::default()
        };
        assert!(matches_cwd("th-1", &h, Some("/w/x"), None));
        assert!(!matches_cwd("th-1", &h, Some("/w/y"), None));
        assert!(matches_cwd("th-1", &Handoff::default(), Some("/w/y"), Some("th-1-feature")));
        assert!(!matches_cwd("th-1", &Handoff::default(), None, Some("main")));
    }

    #[test]
    fn ci_summary_folds_rollup() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(summarize_ci(None), serde_json::Value::Null);
        assert_eq!(summarize_ci(Some(&j("[]"))), serde_json::Value::Null);
        assert_eq!(summarize_ci(Some(&j(r#"[{"conclusion":"SUCCESS"},{"state":"SUCCESS"}]"#))), "success");
        assert_eq!(
            summarize_ci(Some(&j(r#"[{"conclusion":"SUCCESS"},{"conclusion":"","status":"IN_PROGRESS"}]"#))),
            "pending"
        );
        assert_eq!(
            summarize_ci(Some(&j(r#"[{"conclusion":"","status":"QUEUED"},{"conclusion":"FAILURE"}]"#))),
            "failure"
        );
    }

    #[test]
    fn render_mentions_every_section() {
        let packet = serde_json::json!({
            "pearl": {"id": "th-1", "title": "T", "status": "in_progress", "description": "why"},
            "handoff": {"worktree": "/w", "branch": "b", "head": "0123456789abcdef", "dirty": ["M a"], "next": "ship it"},
            "checkpoints": [{"at": "2026-09-07T00:00:00Z", "note": "started", "auto": false}, {"at": "2026-09-07T01:00:00Z", "note": null, "auto": true}],
            "blocks": ["th-2"],
            "pr": {"number": 7, "url": "https://x/7", "ci": "pending"},
        });
        let s = render(&packet);
        for needle in [
            "## th-1 — T",
            "worktree: /w",
            "branch:   b",
            "head:     0123456789ab",
            "M a",
            "#7",
            "ci: pending",
            "blocked by: th-2",
            "started",
            "(auto)",
            "next: ship it",
        ] {
            assert!(s.contains(needle), "missing {needle:?} in:\n{s}");
        }
    }

    // ── store-backed (SQLite in a tempdir; never touches ~/.smooth) ──
    fn test_store() -> PearlStore {
        let tmp = tempfile::tempdir().unwrap();
        let store = PearlStore::open_with_db(&tmp.path().join("pearls.db"), &tmp.path().join("proj")).unwrap();
        std::mem::forget(tmp);
        store
    }

    #[test]
    fn checkpoint_then_packet_round_trips_through_the_store() {
        let store = test_store();
        let p = store
            .create(&smooth_pearls::NewPearl {
                title: "t".into(),
                description: String::new(),
                pearl_type: smooth_pearls::PearlType::Task,
                priority: smooth_pearls::Priority::Medium,
                assigned_to: None,
                parent_id: None,
                labels: vec![],
            })
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        checkpoint(
            &store,
            &p.id,
            tmp.path(),
            &CheckpointOpts {
                note: Some("one".into()),
                next: Some("two".into()),
                ..Default::default()
            },
        )
        .unwrap();
        checkpoint(
            &store,
            &p.id,
            tmp.path(),
            &CheckpointOpts {
                auto: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(checkpoint(&store, "th-nope", tmp.path(), &CheckpointOpts::default()).is_err());
        let pk = packet(&store, &p, false).unwrap();
        assert_eq!(pk["checkpoints"].as_array().unwrap().len(), 2);
        assert_eq!(pk["checkpoints"][0]["note"], "one");
        assert_eq!(pk["checkpoints"][1]["auto"], true);
        assert_eq!(pk["handoff"]["next"], "two");
        assert!(pk["pr"].is_null());
    }
}
