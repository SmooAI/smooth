//! Harness manifests end to end: the per-harness state-source matrix, the
//! sort/hide prefs (PUT → `th harness list` order, `flow.hello` omits
//! hidden), `th harness add <path>` of a custom manifest and a session on it,
//! and — opt-in — the real coding CLIs installed on this machine.

use std::time::Duration;

use serde_json::{json, Value};

use crate::support::{prereqs, prereqs_with_th, skip, state, Daemon, WAIT};

fn names(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|h| h["name"].as_str().unwrap().to_string()).collect()
}

fn by_name<'a>(v: &'a Value, name: &str) -> &'a Value {
    v.as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == name)
        .unwrap_or_else(|| panic!("no harness {name} in {v}"))
}

/// Every state source the engine supports, one fake-agent flavour each:
/// what `Session.state_source` reads once the harness has done a turn.
#[tokio::test]
async fn harness_matrix_state_source_per_manifest() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let (status, v) = d.get("/api/flow/harnesses").await;
    assert_eq!(status, 200);
    let rows = &v["harnesses"];
    // The built-ins are always listed, with their manifest's source, whether
    // or not the binary resolves on this runner.
    for (name, source) in [("claude", "hooks"), ("opencode", "hooks"), ("codex", "hooks"), ("th-code", "native")] {
        let h = by_name(rows, name);
        assert_eq!(h["state_source"], source, "{h}");
        assert_eq!(h["origin"], "builtin");
        assert_eq!(
            h["installed"].is_boolean() && (h["installed"] == true) == h["binary_path"].is_string(),
            true,
            "{h}"
        );
        if h["installed"] == false {
            assert!(h["reason"].as_str().unwrap().contains("not found on PATH"), "{h}");
        }
    }
    let mut matrix = Vec::new();
    for (kind, manifest_source, session_source) in [
        ("fake-agent", "hooks", "hooks"),
        ("fake-agent-learned", "hooks", "hooks"),
        ("fake-agent-native", "native", "native"),
        ("fake-agent-scrape", "scrape", "inferred"),
    ] {
        let h = by_name(rows, kind);
        assert_eq!(h["state_source"], manifest_source, "{h}");
        assert_eq!(h["origin"], "user", "installed from ~/.smooth/harnesses: {h}");
        assert_eq!(h["installed"], true, "{h}");
        assert!(h["binary_path"].as_str().unwrap().ends_with("/.local/bin/fake-agent"), "{h}");
        let s = d.new_session(kind, Some("/work m")).await;
        let id = s["id"].as_str().unwrap().to_string();
        assert_eq!(s["state_source"], "inferred", "before any report every harness is inferred: {s}");
        let idle = d
            .wait_until(&id, "idle", WAIT, |s| state(s) == "idle" && s["state_source"] == session_source)
            .await;
        matrix.push((kind, manifest_source, idle["state_source"].as_str().unwrap().to_string()));
        d.wait_screen(&id, "worked: m", WAIT).await;
        d.kill(&id, false).await;
    }
    eprintln!("state-source matrix (kind, manifest, session): {matrix:?}");
    assert_eq!(
        matrix.iter().map(|(_, _, s)| s.as_str()).collect::<Vec<_>>(),
        ["hooks", "hooks", "native", "inferred"]
    );
    // An unknown kind is refused with the pointer to `th harness list`.
    let (status, v) = d.post("/api/flow/sessions", json!({"kind":"aider","worktree":d.ws})).await;
    assert_eq!(status, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("th harness list"), "{v}");
}

#[tokio::test]
async fn harness_prefs_sort_and_hide_reach_every_picker() {
    if !prereqs_with_th() {
        return;
    }
    let d = Daemon::boot().await;
    let ws0 = d.ws().await;
    let before = names(&ws0.hello["harnesses"]);
    assert_eq!(
        &before[..4],
        ["claude", "opencode", "codex", "th-code"],
        "built-ins first, in registry order: {before:?}"
    );
    assert!(before.contains(&"fake-agent".to_string()));

    // PUT prefs → the reply is the full list (hidden flagged), the WS gets
    // flow.harnesses with the visible list, `th harness list` follows.
    let mut ws = d.ws().await;
    let (status, v) = d
        .put("/api/flow/harnesses/prefs", json!({"order":["fake-agent","th-code"],"hidden":["codex"]}))
        .await;
    assert_eq!(status, 200, "{v}");
    let all = names(&v["harnesses"]);
    assert_eq!(&all[..3], ["fake-agent", "th-code", "claude"], "{all:?}");
    assert_eq!(by_name(&v["harnesses"], "codex")["hidden"], true);
    let bc = ws.wait_for("flow.harnesses", WAIT, |f| f["type"] == "flow.harnesses").await;
    let visible = names(&bc["harnesses"]);
    assert!(!visible.contains(&"codex".to_string()), "{visible:?}");
    assert_eq!(&visible[..2], ["fake-agent", "th-code"]);
    // A fresh hello omits hidden, keeps the order.
    let ws2 = d.ws().await;
    let hello = names(&ws2.hello["harnesses"]);
    assert_eq!(hello, visible);

    let list = d.th_json(&["harness", "list", "--json"]);
    assert_eq!(list["source"], "daemon");
    let shown = names(&list["harnesses"]);
    assert_eq!(&shown[..2], ["fake-agent", "th-code"]);
    assert!(!shown.contains(&"codex".to_string()), "hidden by default: {shown:?}");
    let all = d.th_json(&["harness", "list", "--all", "--json"]);
    assert_eq!(by_name(&all["harnesses"], "codex")["hidden"], true);
    let (_, table, _) = d.th(&["harness", "list", "--all"]);
    assert!(table.contains("codex") && table.contains("hidden"), "{table}");

    // The CLI verbs: unhide / hide / order.
    let (code, _, err) = d.th(&["harness", "unhide", "codex"]);
    assert_eq!(code, 0, "{err}");
    assert!(names(&d.th_json(&["harness", "list", "--json"])["harnesses"]).contains(&"codex".to_string()));
    let (code, _, err) = d.th(&["harness", "hide", "opencode"]);
    assert_eq!(code, 0, "{err}");
    assert!(!names(&d.th_json(&["harness", "list", "--json"])["harnesses"]).contains(&"opencode".to_string()));
    let (code, out, err) = d.th(&["harness", "order", "th-code", "claude"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("th-code"), "{out}");
    let shown = names(&d.th_json(&["harness", "list", "--json"])["harnesses"]);
    assert_eq!(&shown[..2], ["th-code", "claude"], "{shown:?}");
    // Unknown names are refused, by the daemon and by the CLI.
    let (status, v) = d.put("/api/flow/harnesses/prefs", json!({"hidden":["cursor"]})).await;
    assert_eq!(status, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("cursor"), "{v}");
    let (code, _, err) = d.th(&["harness", "hide", "cursor"]);
    assert_ne!(code, 0);
    assert!(err.contains("cursor"), "{err}");
    // Prefs survive in flow.db: a fresh registry read applies them.
    let (_, v) = d.get("/api/flow/harnesses").await;
    assert_eq!(&names(&v["harnesses"])[..2], ["th-code", "claude"]);
    assert_eq!(by_name(&v["harnesses"], "opencode")["hidden"], true);
    let _ = ws0;
}

#[tokio::test]
async fn th_harness_add_installs_a_custom_manifest_the_engine_launches() {
    if !prereqs_with_th() {
        return;
    }
    let d = Daemon::boot().await;
    // A custom manifest: the hooks fake-agent under a new name, from a file
    // outside ~/.smooth (what a user would `th harness add`).
    let base = std::fs::read_to_string(d.home.join(".smooth/harnesses/fake-agent.toml")).unwrap();
    let custom = base
        .replace("name = \"fake-agent\"", "name = \"custom-agent\"")
        .replace("display_name = \"Fake Agent (hooks)\"", "display_name = \"Custom Agent\"");
    let src = d.home.join("custom-agent.toml");
    std::fs::write(&src, &custom).unwrap();

    let (code, out, err) = d.th(&["harness", "add", src.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("custom-agent") && out.contains(".smooth/harnesses/custom-agent.toml"), "{out}");
    assert!(out.contains("/.local/bin/fake-agent"), "the resolved binary is shown: {out}");
    assert!(d.home.join(".smooth/harnesses/custom-agent.toml").is_file());
    // Adding it again refuses without --force.
    let (code, _, err) = d.th(&["harness", "add", src.to_str().unwrap()]);
    assert_ne!(code, 0);
    assert!(err.contains("--force"), "{err}");
    assert_eq!(d.th(&["harness", "add", src.to_str().unwrap(), "--force"]).0, 0);
    // A manifest that fails validation is refused with the field named.
    let bad = d.home.join("bad.toml");
    std::fs::write(&bad, custom.replace("prompt_as = \"argv\"", "prompt_as = \"paste\"")).unwrap();
    let (code, _, err) = d.th(&["harness", "add", bad.to_str().unwrap()]);
    assert_ne!(code, 0);
    assert!(err.contains("prompt") || err.contains("launch.argv"), "{err}");
    assert!(!d.home.join(".smooth/harnesses/bad.toml").exists());

    // No daemon restart: the daemon lists it, `th harness show` reads it,
    // pickers get it on their next hello, and a session launches on it.
    let (_, v) = d.get("/api/flow/harnesses").await;
    let h = by_name(&v["harnesses"], "custom-agent");
    assert_eq!(h["display_name"], "Custom Agent");
    assert_eq!(h["origin"], "user");
    assert_eq!(h["installed"], true, "{h}");
    let show = d.th_json(&["harness", "show", "custom-agent", "--json"]);
    assert_eq!(show["manifest"]["name"], "custom-agent", "{show}");
    assert_eq!(show["origin"], "user");
    assert_eq!(show["installed"], true, "{show}");
    assert!(show["path"].as_str().unwrap().ends_with(".smooth/harnesses/custom-agent.toml"), "{show}");
    let ws = d.ws().await;
    assert!(names(&ws.hello["harnesses"]).contains(&"custom-agent".to_string()));
    let v = d.th_json(&[
        "flow",
        "new",
        "--kind",
        "custom-agent",
        "--worktree",
        d.ws.to_str().unwrap(),
        "--prompt",
        "/work custom",
        "--json",
    ]);
    let id = v["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["session"]["kind"], "custom-agent");
    d.wait_until(&id, "idle via hooks", WAIT, |s| state(s) == "idle" && s["state_source"] == "hooks")
        .await;
    d.wait_screen(&id, "worked: custom", WAIT).await;
    d.kill(&id, false).await;
}

/// Opt-in (`SMOOTH_E2E_REAL_HARNESSES=1`): launch every REAL coding CLI this
/// machine has — claude / opencode / codex where installed, and `th code` —
/// through its built-in manifest, assert the launch shape and the state
/// source the engine reads, then kill it. No credentials are needed: the
/// engine's side of the contract holds before the CLI ever talks to a model.
#[tokio::test]
async fn real_harnesses_launch_through_their_manifests() {
    if !prereqs() {
        return;
    }
    if !std::env::var("SMOOTH_E2E_REAL_HARNESSES").is_ok_and(|v| !v.is_empty() && v != "0") {
        skip("SMOOTH_E2E_REAL_HARNESSES is not set — real claude/opencode/codex/th-code launches are opt-in");
        return;
    }
    let d = Daemon::boot().await;
    let (_, v) = d.get("/api/flow/harnesses").await;
    let mut matrix = Vec::new();
    for name in ["claude", "opencode", "codex", "th-code"] {
        let h = by_name(&v["harnesses"], name);
        if h["installed"] != true {
            eprintln!("[skip] {name}: {}", h["reason"]);
            continue;
        }
        let s = d.new_session(name, Some("say ok and stop")).await;
        let id = s["id"].as_str().unwrap().to_string();
        assert_eq!(s["argv"][0], h["binary_path"], "{s}");
        assert_eq!(s["state_source"], "inferred");
        match name {
            "claude" => {
                assert_eq!(s["argv"][1], "--session-id");
                assert_eq!(s["argv"][2], s["agent_session_id"]);
            }
            "opencode" => assert!(s["argv"].as_array().unwrap().iter().any(|a| a == "--prompt"), "{s}"),
            "codex" => assert!(s["agent_session_id"].is_null(), "learned: {s}"),
            "th-code" => {
                assert_eq!(s["argv"][1], "code");
                assert!(s["agent_session_id"].is_string(), "{s}");
            }
            _ => unreachable!(),
        }
        // Without credentials a real CLI paints onboarding / sign-in in the
        // fresh HOME — a pane no scrape pattern matches, so `starting` is the
        // engine's honest reading. What is provable creds-free: the process
        // came up and painted, its pid is live, the row's shape is right;
        // th code additionally reports natively (its first turn_start
        // happens before any model call).
        let painted = d.wait_screen_nonblank(&id, Duration::from_secs(60)).await;
        assert!(!painted.trim().is_empty());
        let live = d.session(&id).await;
        assert!(Daemon::pid_alive(live["pid"].as_u64().unwrap() as u32), "{live}");
        assert_ne!(state(&live), "dead", "{live}");
        if name == "th-code" {
            d.wait_until(&id, "native report", Duration::from_secs(60), |s| s["state_source"] == "native")
                .await;
        }
        let live = d.session(&id).await;
        matrix.push((
            name,
            live["state"].as_str().unwrap().to_string(),
            live["state_source"].as_str().unwrap().to_string(),
        ));
        let killed = d.kill(&id, false).await;
        assert_eq!(state(&killed), "done", "{killed}");
    }
    eprintln!("real harness matrix (kind, state, source): {matrix:?}");
    assert!(!matrix.is_empty(), "SMOOTH_E2E_REAL_HARNESSES is set but no real harness is installed");
}
