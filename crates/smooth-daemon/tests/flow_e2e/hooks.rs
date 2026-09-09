//! The `POST /api/flow/hooks` contract, event by event, against a live
//! session — what `flow-hook.sh` (the smooth-agent plugin) and the harness
//! plugins post, and what the engine does with each.

use std::time::Duration;

use serde_json::{json, Value};

use crate::support::{prereqs, state, Daemon, Ws, WAIT};

/// The next `flow.event` for `id` whose text contains `needle`.
async fn expect_event(ws: &mut Ws, id: &str, kind: &str, needle: &str) -> Value {
    let what = format!("flow.event {kind} …{needle}…");
    ws.wait_for(&what, WAIT, |v| {
        v["type"] == "flow.event" && v["id"] == id && v["kind"] == kind && v["text"].as_str().is_some_and(|t| t.contains(needle))
    })
    .await
}

#[tokio::test]
async fn hooks_contract_per_event() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let mut ws = d.ws().await;
    // A live agent that just sits at its prompt — the hooks below are what
    // ITS hook script would post, with its pre-assigned id.
    let s = d.new_session("fake-agent", None).await;
    let id = s["id"].as_str().unwrap().to_string();
    let agent = s["agent_session_id"].as_str().unwrap().to_string();
    let cwd = d.ws.to_string_lossy().into_owned();
    let idle = d.wait_state(&id, "idle", WAIT).await;
    assert_eq!(idle["state_source"], "hooks", "fake-agent's SessionStart already flipped the source: {idle}");
    let hook = |event: &'static str, payload: Value| {
        let d = &d;
        let agent = agent.clone();
        let cwd = cwd.clone();
        async move {
            let (status, body) = d.hook("claude-code", event, &agent, Some(&cwd), payload).await;
            assert_eq!(status, 200, "{event}: {body}");
            body
        }
    };

    // SessionStart / PreCompact / SubagentStop / anything unknown: no state change.
    for ev in ["SessionStart", "PreCompact", "SubagentStop", "SomethingNew"] {
        assert_eq!(hook(ev, json!({"source":"startup"})).await, json!({}));
    }
    assert_eq!(state(&d.session(&id).await), "idle");

    // UserPromptSubmit → working + the user line.
    assert_eq!(hook("UserPromptSubmit", json!({"prompt":"  fix it  "})).await, json!({}));
    d.wait_state(&id, "working", WAIT).await;
    expect_event(&mut ws, &id, "user", "fix it").await;
    expect_event(&mut ws, &id, "system", "working").await;

    // PreToolUse → working + the tool line; PostToolUse → working, no line.
    hook("PreToolUse", json!({"tool_name":"Bash","tool_input":{"command":"ls -la"}})).await;
    expect_event(&mut ws, &id, "tool", "Bash(ls -la)").await;
    hook("PostToolUse", json!({"tool_name":"Bash","tool_response":{"stdout":"x"}})).await;
    assert_eq!(state(&d.session(&id).await), "working");

    // Stop → idle, unread, the agent's last message.
    hook("Stop", json!({"last_assistant_message":"all done"})).await;
    let s = d.wait_until(&id, "idle+unread", WAIT, |s| state(s) == "idle" && s["unread"] == true).await;
    assert!(s["attention"].is_null());
    expect_event(&mut ws, &id, "agent", "all done").await;
    expect_event(&mut ws, &id, "system", "idle").await;

    // Notification(permission) → needs_you · permission, detail = message,
    // no request_id (nothing to long-poll); the message is a system line.
    hook(
        "Notification",
        json!({"notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}),
    )
    .await;
    let p = d.wait_state(&id, "needs_you", WAIT).await;
    assert_eq!(p["attention"]["reason"], "permission");
    assert_eq!(p["attention"]["detail"], "Claude needs your permission to use Bash");
    assert!(p["attention"]["request_id"].is_null(), "{p}");
    expect_event(&mut ws, &id, "system", "needs your permission").await;
    // approve with no pending request presses the key on the pane → working.
    // fake-agent reads whole lines, so an empty steer (just Enter) shows the
    // `1` the keystroke path typed.
    d.approve(&id, "whatever", "allow").await;
    d.wait_state(&id, "working", WAIT).await;
    d.send(&id, "x").await;
    d.wait_screen(&id, "echo: 1x", WAIT).await;

    // Notification(question) → needs_you · question.
    hook(
        "Notification",
        json!({"notification_type":"idle_prompt","message":"Claude is waiting for your input"}),
    )
    .await;
    let q = d.wait_state(&id, "needs_you", WAIT).await;
    assert_eq!(q["attention"]["reason"], "question");
    d.approve(&id, "whatever", "deny").await;
    d.wait_state(&id, "working", WAIT).await;

    // Any other notification: a line, no state change.
    hook("Notification", json!({"notification_type":"other","message":"fyi only"})).await;
    expect_event(&mut ws, &id, "system", "fyi only").await;
    assert_eq!(state(&d.session(&id).await), "working");

    // SessionEnd: the exit is decided by the PTY, not the hook — state holds,
    // the line lands.
    hook("SessionEnd", json!({"reason":"exit"})).await;
    expect_event(&mut ws, &id, "system", "session ended (exit)").await;
    assert_eq!(state(&d.session(&id).await), "working");

    // Unknown session: quiet 200 {} (the hook script must never block).
    let (status, body) = d.hook("claude-code", "Stop", "nobody-here", Some(&cwd), json!({})).await;
    assert_eq!((status, body), (200, json!({})));
    // Missing session_id: also never a 5xx.
    let r = reqwest::Client::new()
        .post(d.url("/api/flow/hooks"))
        .header("content-type", "application/json")
        .body("not json at all")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_client_error(), "malformed body: {}", r.status());

    d.kill(&id, false).await;
}

#[tokio::test]
async fn permission_request_long_polls_until_approved_each_decision_shape() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    let s = d.new_session("fake-agent", None).await;
    let id = s["id"].as_str().unwrap().to_string();
    let agent = s["agent_session_id"].as_str().unwrap().to_string();
    d.wait_state(&id, "idle", WAIT).await;

    for (decision, want_behavior) in [("deny", "deny"), ("allow", "allow"), ("allow_session", "allow")] {
        let (dd, aa) = (d.url("/api/flow/hooks"), agent.clone());
        let post = tokio::spawn(async move {
            reqwest::Client::new()
                .post(dd)
                .json(&json!({"harness":"claude-code","event":"PermissionRequest","session_id":aa,
                    "payload":{"tool_name":"Bash","tool_input":{"command":"git push"}}}))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        });
        let ask = d
            .wait_until(&id, "needs_you with request_id", WAIT, |s| {
                state(s) == "needs_you" && s["attention"]["request_id"].is_string()
            })
            .await;
        let request_id = ask["attention"]["request_id"].as_str().unwrap().to_string();
        assert_eq!(ask["attention"]["detail"], "Bash: git push");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!post.is_finished(), "held open until a decision");

        // A Notification for the same prompt keeps the request_id.
        d.hook(
            "claude-code",
            "Notification",
            &agent,
            None,
            json!({"notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}),
        )
        .await;
        assert_eq!(d.session(&id).await["attention"]["request_id"], request_id);

        // A wrong request_id falls through to the keystroke path (the row is
        // live, so it succeeds) but does NOT answer the long-poll.
        d.approve(&id, "not-this-one", "allow").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!post.is_finished(), "a mismatched request_id must not resolve the hook");

        d.approve(&id, &request_id, decision).await;
        let body = tokio::time::timeout(WAIT, post).await.unwrap().unwrap();
        assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PermissionRequest", "{body}");
        assert_eq!(body["hookSpecificOutput"]["decision"]["behavior"], want_behavior, "{body}");
        match decision {
            "deny" => assert!(body["hookSpecificOutput"]["decision"]["message"].is_string()),
            "allow_session" => {
                let rule = &body["hookSpecificOutput"]["decision"]["updatedPermissions"][0];
                assert_eq!(rule["rules"][0]["toolName"], "Bash");
                assert_eq!(rule["destination"], "session");
            }
            _ => assert!(body["hookSpecificOutput"]["decision"]["updatedPermissions"].is_null()),
        }
        d.wait_state(&id, "working", WAIT).await;
    }
    d.kill(&id, false).await;
}

#[tokio::test]
async fn hooks_are_unauthenticated_and_everything_else_is_gated() {
    if !prereqs() {
        return;
    }
    let d = Daemon::boot().await;
    assert_eq!(d.get_unauthed("/api/flow/sessions").await, 401);
    assert_eq!(d.get_unauthed("/api/flow/harnesses").await, 401);
    assert_eq!(d.get_unauthed("/api/flow/sessions/fs-nope/snapshot").await, 401);
    let (status, _) = d.get("/api/flow/sessions").await;
    assert_eq!(status, 200);
    // Bearer + ?token= are the other two spellings.
    let http = reqwest::Client::new();
    let r = http.get(d.url("/api/flow/sessions")).bearer_auth(&d.token).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = http.get(format!("{}?token={}", d.url("/api/flow/sessions"), d.token)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    // Hooks: no token, still 200.
    let (status, body) = d.hook("claude-code", "Stop", "nobody", None, json!({})).await;
    assert_eq!((status, body), (200, json!({})));
    // The WS handshake is refused without the token.
    assert!(tokio_tungstenite::connect_async(format!("ws://{}/api/flow/ws", d.addr)).await.is_err());
    assert!(tokio_tungstenite::connect_async(format!("ws://{}/api/flow/ws?token=nope", d.addr))
        .await
        .is_err());
    // Unknown session on a gated route → 404 with an error object.
    let (status, v) = d.get("/api/flow/sessions/fs-nope/snapshot").await;
    assert_eq!(status, 404, "{v}");
    assert!(v["error"].is_string());
}
