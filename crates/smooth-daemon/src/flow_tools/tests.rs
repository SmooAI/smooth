//! The in-process flow tools against a real engine on a scratch `$HOME`, with
//! sessions in smooth-flow's in-memory `FakeHost` (nothing runs, no tmux).
//! State changes a harness would report through hooks are written straight
//! to the engine's `flow.db` from a second store handle.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use smooth_flow::host::fake::FakeHost;
use smooth_flow::{Attention, Engine, EngineConfig, FlowStore, SessionRef, SessionState};
use smooth_operator::Tool;

use super::*;

/// `infer::gather` shells out to `th pearls` for any session in a git repo,
/// and a real `th` would open the real `~/.smooth/pearls.db`. Point it at a
/// binary that doesn't exist, once, for the whole test process. (Everything
/// else that resolves `th` falls through a missing `SMOOTH_TH_BIN`.)
fn no_real_th() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| std::env::set_var("SMOOTH_TH_BIN", "/definitely/not/a/th/binary"));
}

struct Rig {
    _tmp: tempfile::TempDir,
    /// The tempdir, canonical (macOS `/var` → `/private/var`), so paths git
    /// reports back compare equal to the ones the test built.
    root: std::path::PathBuf,
    engine: Engine,
    host: Arc<FakeHost>,
    tools: Vec<Arc<dyn Tool>>,
}

impl Rig {
    fn new() -> Self {
        no_real_th();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let work = root.join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let host = FakeHost::new();
        let mut cfg = EngineConfig::new(work.clone());
        cfg.db_path = root.join("flow.db");
        cfg.home = home;
        cfg.daemon_url = Some("http://127.0.0.1:1".into());
        cfg.host = host.clone();
        let engine = Engine::open(cfg).unwrap();
        let tools = flow_tools_with_timing(
            &engine,
            &work,
            WaitTiming {
                poll: Duration::from_millis(40),
                stall_after: Duration::from_millis(1500),
            },
        );
        Self {
            _tmp: tmp,
            root,
            engine,
            host,
            tools,
        }
    }

    fn work(&self) -> std::path::PathBuf {
        self.root.join("work")
    }

    fn tool(&self, name: &str) -> &Arc<dyn Tool> {
        self.tools.iter().find(|t| t.schema().name == name).unwrap_or_else(|| panic!("no tool {name}"))
    }

    async fn call(&self, name: &str, args: Value) -> anyhow::Result<String> {
        self.tool(name).execute(args).await
    }

    async fn ok(&self, name: &str, args: Value) -> String {
        self.call(name, args).await.unwrap_or_else(|e| panic!("{name} failed: {e:#}"))
    }

    /// Start a session and return its id.
    async fn start(&self, args: Value) -> String {
        let out = self.ok("flow_new", args).await;
        started_id(&out)
    }

    /// What a hook would report: write the state straight into flow.db.
    fn set_state(&self, id: &str, state: SessionState, attention: Option<&Attention>) {
        FlowStore::open(&self.root.join("flow.db")).unwrap().set_state(id, state, attention).unwrap();
    }

    fn state(&self, id: &str) -> SessionState {
        self.engine.get(id).unwrap().unwrap().state
    }

    fn pane(&self, id: &str) -> SessionRef {
        let s = self.engine.get(id).unwrap().unwrap();
        SessionRef::new(s.tmux_socket.unwrap(), s.tmux_session.unwrap())
    }

    fn typed(&self, id: &str) -> Vec<String> {
        self.host.session(&self.pane(id)).map(|f| f.input).unwrap_or_default()
    }
}

fn started_id(out: &str) -> String {
    out.lines()
        .next()
        .and_then(|l| l.strip_prefix("Started "))
        .unwrap_or_else(|| panic!("no session id in {out}"))
        .trim()
        .to_string()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repo with one commit at `dir/src`, and a bare clone of it at
/// `dir/origin.git` — the "remote" `project_setup` clones.
fn bare_origin(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    std::fs::write(src.join("README"), "hi").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-qm", "init"]);
    let origin = dir.join("origin.git");
    git(dir, &["clone", "-q", "--bare", &src.to_string_lossy(), &origin.to_string_lossy()]);
    origin
}

// ── the vocabulary ────────────────────────────────────────────────────────────

#[test]
fn every_tool_is_registered_once_in_exactly_one_permission_class() {
    let rig = Rig::new();
    let names: Vec<String> = rig.tools.iter().map(|t| t.schema().name).collect();
    assert_eq!(names.len(), 15);
    let mut classed: Vec<&str> = FLOW_READ_TOOLS
        .iter()
        .chain(FLOW_WRITE_TOOLS)
        .chain(FLOW_DESTRUCTIVE_TOOLS)
        .copied()
        .chain(std::iter::once(FLOW_APPROVE_TOOL))
        .collect();
    classed.sort_unstable();
    let mut sorted: Vec<&str> = names.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    assert_eq!(sorted, classed, "every tool has exactly one class");

    for t in &rig.tools {
        let schema = t.schema();
        let read = FLOW_READ_TOOLS.contains(&schema.name.as_str());
        assert_eq!(t.is_read_only(), read, "{} read-only flag", schema.name);
        assert_eq!(t.is_concurrent_safe(), read, "{} concurrency", schema.name);
        assert_eq!(schema.parameters["type"], "object", "{} schema", schema.name);
        assert!(!schema.description.is_empty());
    }
    // The MCP vocabulary: the same required fields the MCP arg structs have.
    let required = |n: &str| rig.tool(n).schema().parameters["required"].clone();
    assert_eq!(required("flow_send"), json!(["id", "text"]));
    assert_eq!(required("flow_approve"), json!(["id", "decision"]));
    assert_eq!(required("flow_fanout_new"), json!(["prompt", "pearl_id", "candidates"]));
    assert_eq!(required("flow_fanout_pick"), json!(["fan_out_id", "winner_session_id"]));
    assert_eq!(required("project_setup"), json!(["repo"]));
    assert!(rig.tool("flow_approve").schema().description.contains("explicit consent"));
}

// ── read tools + flow_new ─────────────────────────────────────────────────────

#[tokio::test]
async fn list_new_snapshot_harnesses_and_infer() {
    let rig = Rig::new();
    assert!(rig.ok("flow_list", json!({})).await.contains("No SmoothFlow sessions"));

    let work = rig.work().to_string_lossy().into_owned();
    let id = rig.start(json!({"kind": "shell", "directory": work, "title": "a shell"})).await;
    assert!(id.starts_with("fs-"), "{id}");

    let list = rig.ok("flow_list", Value::Null).await;
    assert!(list.starts_with("1 session(s)") && list.contains(&id) && list.contains("a shell"), "{list}");
    assert!(rig.ok("flow_list", json!({"state": "working"})).await.contains("No SmoothFlow sessions"));

    assert_eq!(rig.ok("flow_snapshot", json!({"id": id})).await, "(empty screen)");
    rig.host.set_screen(&rig.pane(&id), "$ echo hi\nhi\n");
    assert!(rig.ok("flow_snapshot", json!({"id": id})).await.contains("hi"));

    let harnesses = rig.ok("flow_harnesses", json!({})).await;
    assert!(harnesses.contains("- claude ("), "the built-ins are listed: {harnesses}");

    // No repo index on a scratch engine: an honest empty answer.
    assert_eq!(rig.ok("flow_repos", json!({"query": "x"})).await, "No repo matches.");

    let inferred: Value = serde_json::from_str(&rig.ok("flow_infer", json!({"cwd": "."})).await).unwrap();
    assert!(inferred.is_object(), "{inferred}");

    let handoff: Value = serde_json::from_str(&rig.ok("flow_handoff", json!({"id": id})).await).unwrap();
    assert_eq!(handoff["handoff"]["worktree"], json!(work));

    let err = rig.call("flow_snapshot", json!({"id": "fs-nope"})).await.unwrap_err();
    assert!(format!("{err:#}").contains("no such session"), "{err:#}");
    assert!(rig.call("flow_snapshot", json!({})).await.is_err(), "missing id is an argument error");
}

#[tokio::test]
async fn flow_new_starts_an_agent_with_its_prompt_and_rejects_a_bad_kind() {
    let rig = Rig::new();
    let out = rig
        .ok(
            "flow_new",
            json!({"directory": rig.work().to_string_lossy(), "prompt": "fix the parser", "title": "parser fix"}),
        )
        .await;
    let id = started_id(&out);
    assert!(out.contains("[claude]"), "claude is the default kind: {out}");
    let s = rig.engine.get(&id).unwrap().unwrap();
    assert_eq!(s.title, "parser fix");
    assert!(rig.call("flow_new", json!({"kind": "Not A Kind!"})).await.is_err());
    assert!(rig
        .call("flow_new", json!({"kind": "shell", "directory": "/definitely/not/here"}))
        .await
        .is_err());
}

// ── send + prompt_wait ────────────────────────────────────────────────────────

#[tokio::test]
async fn send_types_into_the_session() {
    let rig = Rig::new();
    let id = rig.start(json!({"kind": "shell", "directory": rig.work().to_string_lossy()})).await;
    assert_eq!(rig.ok("flow_send", json!({"id": id, "text": "ls"})).await, format!("Sent to {id}."));
    assert!(rig.typed(&id).iter().any(|i| i == "text:ls"), "{:?}", rig.typed(&id));
}

#[tokio::test]
async fn prompt_wait_settles_when_the_turn_goes_working_then_idle() {
    let rig = Rig::new();
    let id = rig.start(json!({"directory": rig.work().to_string_lossy()})).await;
    rig.set_state(&id, SessionState::Idle, None);
    rig.host.set_screen(&rig.pane(&id), "● Done: parser fixed\n> ");

    let db = rig.root.join("flow.db");
    let sid = id.clone();
    let (host, pane) = (rig.host.clone(), rig.pane(&id));
    let agent = tokio::spawn(async move {
        // The harness starts working once the prompt has landed (a loaded
        // machine can take a while to paste it), then finishes.
        while !host.session(&pane).is_some_and(|f| f.input.iter().any(|i| i == "paste:fix it")) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let st = FlowStore::open(&db).unwrap();
        st.set_state(&sid, SessionState::Working, None).unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        st.set_state(&sid, SessionState::Idle, None).unwrap();
    });
    let out = rig.ok("flow_prompt_wait", json!({"id": id, "text": "fix it", "timeout_secs": 30})).await;
    agent.await.unwrap();
    assert!(out.starts_with("Turn ended:") && out.contains("idle"), "{out}");
    assert!(out.contains("parser fixed"), "the screen tail comes back: {out}");
    assert!(
        rig.typed(&id).iter().any(|i| i == "paste:fix it"),
        "the prompt was pasted: {:?}",
        rig.typed(&id)
    );
}

#[tokio::test]
async fn prompt_wait_reports_a_stall_when_the_agent_never_starts() {
    let rig = Rig::new();
    let id = rig.start(json!({"directory": rig.work().to_string_lossy()})).await;
    rig.set_state(&id, SessionState::Idle, None);
    let out = rig.ok("flow_prompt_wait", json!({"id": id, "text": "hello?"})).await;
    assert!(out.contains("never started working"), "{out}");
}

#[tokio::test]
async fn prompt_wait_ends_when_the_agent_blocks_on_an_approval() {
    let rig = Rig::new();
    let id = rig.start(json!({"directory": rig.work().to_string_lossy()})).await;
    rig.set_state(&id, SessionState::Working, None);
    let db = rig.root.join("flow.db");
    let sid = id.clone();
    let agent = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut att = Attention::new("permission");
        att.request_id = Some("r-7".into());
        FlowStore::open(&db).unwrap().set_state(&sid, SessionState::NeedsYou, Some(&att)).unwrap();
    });
    let out = rig.ok("flow_prompt_wait", json!({"id": id, "text": "go"})).await;
    agent.await.unwrap();
    assert!(out.contains("needs_you") && out.contains("request_id r-7"), "{out}");
}

#[tokio::test]
async fn prompt_wait_refuses_a_blocked_agent_without_typing_anything() {
    let rig = Rig::new();
    let id = rig.start(json!({"directory": rig.work().to_string_lossy()})).await;
    for (state, hint) in [
        (SessionState::NeedsYou, "flow_approve"),
        (SessionState::Limited, "limited"),
        (SessionState::Dead, "dead"),
    ] {
        rig.set_state(&id, state, Some(&Attention::new("permission")));
        let before = rig.typed(&id);
        let err = rig.call("flow_prompt_wait", json!({"id": id, "text": "nope"})).await.unwrap_err();
        assert!(format!("{err}").contains(hint), "{state:?}: {err}");
        assert_eq!(rig.typed(&id), before, "nothing typed into a {state:?} agent");
        if state == SessionState::Dead {
            break; // terminal: no transitions out
        }
    }
}

// ── approve / kill / close / fan-out ──────────────────────────────────────────

#[tokio::test]
async fn approve_answers_the_pending_request_and_validates_its_input() {
    let rig = Rig::new();
    let id = rig.start(json!({"directory": rig.work().to_string_lossy()})).await;

    let err = rig.call("flow_approve", json!({"id": id, "decision": "allow"})).await.unwrap_err();
    assert!(format!("{err}").contains("no pending approval"), "{err}");
    let err = rig.call("flow_approve", json!({"id": id, "decision": "sure"})).await.unwrap_err();
    assert!(format!("{err}").contains("allow, deny or allow_session"), "{err}");

    let mut att = Attention::new("permission");
    att.request_id = Some("r-1".into());
    rig.set_state(&id, SessionState::NeedsYou, Some(&att));
    let out = rig.ok("flow_approve", json!({"id": id, "decision": "allow"})).await;
    assert_eq!(out, format!("Answered {id} with allow."));
    assert_eq!(rig.state(&id), SessionState::Working, "an answered prompt resumes the turn");
    assert!(
        rig.typed(&id).iter().any(|i| i.starts_with("key:")),
        "a scraped approval menu gets its keys: {:?}",
        rig.typed(&id)
    );
}

#[tokio::test]
async fn kill_stops_the_process_and_close_drops_the_session() {
    let rig = Rig::new();
    let dir = rig.work().to_string_lossy().into_owned();
    let a = rig.start(json!({"kind": "shell", "directory": dir})).await;
    let pid = rig.engine.get(&a).unwrap().unwrap().pid.unwrap();
    assert_eq!(rig.ok("flow_kill", json!({"id": a})).await, format!("{a} stopped."));
    assert_eq!(rig.state(&a), SessionState::Done);
    assert!(rig.host.calls().contains(&format!("kill_process_tree {pid}")));

    let b = rig.start(json!({"kind": "shell", "directory": dir})).await;
    let out = rig.ok("flow_close", json!({"id": b})).await;
    assert!(out.starts_with(&format!("Closed {b}.")), "{out}");
    assert!(rig.engine.get(&b).unwrap().is_none(), "closed sessions leave the fleet");
    assert!(rig.call("flow_kill", json!({"id": "fs-nope"})).await.is_err());
}

#[tokio::test]
async fn fanout_errors_surface_as_tool_errors() {
    let rig = Rig::new();
    let err = rig
        .call(
            "flow_fanout_new",
            json!({"prompt": "p", "pearl_id": "th-1", "candidates": [{"label": "a", "kind": "Bad Kind"}]}),
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("invalid session kind"), "{err}");
    assert!(rig.call("flow_fanout_new", json!({"prompt": "p"})).await.is_err(), "missing fields");
    assert!(rig
        .call("flow_fanout_pick", json!({"fan_out_id": "fo-nope", "winner_session_id": "fs-nope"}))
        .await
        .is_err());
}

// ── project_setup ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn project_setup_clones_a_url_then_reuses_the_clone() {
    let rig = Rig::new();
    let origin = bare_origin(&rig.root);
    let url = format!("file://{}", origin.display());
    let dev = rig.root.join("dev");

    let out = rig
        .ok(
            "project_setup",
            json!({"repo": url, "clone_into": dev.to_string_lossy(), "kind": "shell", "title": "look around"}),
        )
        .await;
    let checkout = dev.join("origin");
    assert!(checkout.join("README").is_file(), "cloned: {out}");
    assert!(out.contains("cloned from"), "{out}");
    let id = out.lines().find_map(|l| l.strip_prefix("Started ")).unwrap().trim().to_string();
    assert_eq!(rig.engine.get(&id).unwrap().unwrap().worktree, checkout.to_string_lossy());

    let again = rig
        .ok("project_setup", json!({"repo": url, "clone_into": dev.to_string_lossy(), "kind": "shell"}))
        .await;
    assert!(again.contains("already cloned from"), "{again}");

    // A different repo that would land on the same directory is refused.
    std::fs::create_dir_all(rig.root.join("other")).unwrap();
    let other_src = rig.root.join("other");
    git(&other_src, &["init", "-q", "--bare", "origin.git"]);
    let err = rig
        .call(
            "project_setup",
            json!({"repo": format!("file://{}", other_src.join("origin.git").display()), "clone_into": dev.to_string_lossy(), "kind": "shell"}),
        )
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("already exists and is not a clone"), "{err}");
}

#[tokio::test]
async fn project_setup_clones_under_the_engines_home_dev_by_default() {
    let rig = Rig::new();
    let origin = bare_origin(&rig.root);
    // A bare repo PATH (not a URL) is a clone source too.
    let out = rig.ok("project_setup", json!({"repo": origin.to_string_lossy(), "kind": "shell"})).await;
    let checkout = rig.engine.home().join("dev").join("origin");
    assert!(checkout.join("README").is_file(), "{out}");
    assert!(out.contains(&checkout.to_string_lossy().into_owned()), "{out}");
}

#[tokio::test]
async fn project_setup_makes_a_branch_or_pearl_worktree_and_starts_the_agent_there() {
    let rig = Rig::new();
    let origin = bare_origin(&rig.root);
    let dev = rig.root.join("dev");
    rig.ok(
        "project_setup",
        json!({"repo": format!("file://{}", origin.display()), "clone_into": dev.to_string_lossy(), "kind": "shell"}),
    )
    .await;
    let checkout = dev.join("origin");

    // A branch: a worktree beside the checkout, on exactly that branch.
    let out = rig
        .ok(
            "project_setup",
            json!({"repo": checkout.to_string_lossy(), "branch": "feat/login", "prompt": "fix the login redirect"}),
        )
        .await;
    let wt = dev.join("origin-feat-login");
    assert_eq!(git_out(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "feat/login", "{out}");
    let id = out.lines().find_map(|l| l.strip_prefix("Started ")).unwrap().trim().to_string();
    let s = rig.engine.get(&id).unwrap().unwrap();
    assert_eq!(s.worktree, wt.to_string_lossy());
    assert_eq!(s.kind.as_str(), "claude", "claude by default");
    assert!(out.contains("don't prompt it again"), "a prompted agent is already working: {out}");

    // A pearl: the engine's own ../<repo>-<pearl>-<slug> worktree.
    let out = rig
        .ok(
            "project_setup",
            json!({"repo": checkout.to_string_lossy(), "pearl_id": "th-abc123", "title": "Fix login", "kind": "shell"}),
        )
        .await;
    let wt = dev.join("origin-th-abc123-fix-login");
    assert!(wt.is_dir(), "{out}");
    assert_eq!(git_out(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "th-abc123-fix-login");
    let id = out.lines().find_map(|l| l.strip_prefix("Started ")).unwrap().trim().to_string();
    assert_eq!(rig.engine.get(&id).unwrap().unwrap().pearl_id.as_deref(), Some("th-abc123"));
}

#[tokio::test]
async fn project_setup_refuses_what_it_cannot_safely_do() {
    let rig = Rig::new();
    let plain = rig.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    for (args, why) in [
        (json!({"repo": "/definitely/not/here"}), "no such directory"),
        (json!({"repo": "ext::sh -c touch% /tmp/pwned"}), "no such directory"),
        (json!({"repo": ""}), "repo is required"),
        (json!({"repo": plain.to_string_lossy(), "pearl_id": "-rf"}), "not a pearl id"),
        (json!({"repo": plain.to_string_lossy(), "branch": "x"}), "not a git repository"),
        (json!({"repo": plain.to_string_lossy(), "kind": "Nope Nope"}), "invalid session kind"),
    ] {
        let err = rig.call("project_setup", args.clone()).await.unwrap_err();
        assert!(format!("{err:#}").contains(why), "{args}: {err:#}");
    }
    // A plain directory with nothing to branch is fine: the agent runs there.
    let out = rig.ok("project_setup", json!({"repo": plain.to_string_lossy(), "kind": "shell"})).await;
    assert!(out.contains("Started fs-"), "{out}");
}

// ── pure helpers ──────────────────────────────────────────────────────────────

#[test]
fn only_ordinary_git_transports_count_as_urls() {
    for url in [
        "https://github.com/SmooAI/smooth.git",
        "http://host/r",
        "ssh://git@github.com/SmooAI/smooth",
        "git://host/r.git",
        "file:///tmp/r.git",
        "git@github.com:SmooAI/smooth.git",
    ] {
        assert!(is_git_url(url), "{url}");
    }
    for not in ["ext::sh -c id", "/abs/path", "~/dev/x", "rel/path", "user@host", "fd::3", "a b@c:d"] {
        assert!(!is_git_url(not), "{not}");
    }
}

#[test]
fn a_clone_is_named_after_the_last_segment() {
    assert_eq!(repo_name("https://github.com/SmooAI/smooth.git").unwrap(), "smooth");
    assert_eq!(repo_name("git@github.com:SmooAI/smooth-operator").unwrap(), "smooth-operator");
    assert_eq!(repo_name("file:///tmp/x/origin.git/").unwrap(), "origin");
    assert_eq!(repo_name("/tmp/bare.git").unwrap(), "bare");
    for bad in ["https://h/..", "https://h/-oops", "https://h/.git", "https://h/a%20b"] {
        assert!(repo_name(bad).is_err(), "{bad}");
    }
}

#[test]
fn remotes_compare_modulo_git_suffix_and_slash() {
    assert!(same_remote("https://h/a.git", "https://h/a"));
    assert!(same_remote("https://h/a/", "https://h/a.git"));
    assert!(!same_remote("", ""));
    assert!(!same_remote("https://h/a", "https://h/b"));
}

#[test]
fn pearl_ids_are_plain_names() {
    assert!(valid_pearl_id("th-8b3918"));
    assert!(valid_pearl_id("SMOODEV-123"));
    for bad in ["", "-x", "a b", "a/b", "../x"] {
        assert!(!valid_pearl_id(bad), "{bad}");
    }
}
