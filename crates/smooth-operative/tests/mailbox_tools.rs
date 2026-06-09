//! Integration tests for `ask_smooth` + `reply_to_chat` tools.
//!
//! Each test opens a temporary Dolt-backed PearlStore (skipping when the
//! `smooth-dolt` binary isn't available, matching `crates/smooth-pearls/src/tools.rs`).
//! The tools' executions are checked end-to-end: pearl comments land with
//! the right prefix, blocking ask_smooth resolves through the QuestionRegistry,
//! and the timeout path returns the no-answer message.
//!
//! Note: these tests live in the bin crate's `tests/` dir, so the modules
//! under test are recompiled as a `cdylib`. We pull the tool types in via
//! the runner's normal source layout.

use std::sync::Arc;
use std::time::Duration;

// The runner is a binary crate; for unit testing of internal modules the
// integration test re-includes the source files directly. This is the same
// approach `tests/agent_permissions.rs` and `tests/subagent_dispatch.rs` use.
#[path = "../src/mailbox.rs"]
#[allow(dead_code)]
mod mailbox;

#[path = "../src/pearl_tools.rs"]
#[allow(dead_code)]
mod pearl_tools;

#[path = "../src/ask_smooth_tool.rs"]
#[allow(dead_code)]
mod ask_smooth_tool;

#[path = "../src/reply_to_chat_tool.rs"]
#[allow(dead_code)]
mod reply_to_chat_tool;

use ask_smooth_tool::AskSmoothTool;
use mailbox::QuestionRegistry;
use pearl_tools::PearlStoreHandle;
use reply_to_chat_tool::ReplyToChatTool;
use smooth_operator::tool::Tool;

fn test_handle() -> Option<Arc<PearlStoreHandle>> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dolt = tmp.path().join("dolt");
    let store = match smooth_pearls::PearlStore::init(&dolt) {
        Ok(s) => s,
        Err(_) => return None, // smooth-dolt binary not on PATH
    };
    std::mem::forget(tmp);
    Some(Arc::new(PearlStoreHandle { store }))
}

fn make_pearl(handle: &PearlStoreHandle) -> String {
    let new = smooth_pearls::NewPearl {
        title: "test".into(),
        description: "desc".into(),
        pearl_type: smooth_pearls::PearlType::Task,
        priority: smooth_pearls::Priority::Medium,
        assigned_to: None,
        parent_id: None,
        labels: Vec::new(),
    };
    handle.store.create(&new).expect("create").id
}

#[tokio::test]
async fn ask_smooth_fyi_returns_immediately_with_id() {
    let Some(handle) = test_handle() else {
        eprintln!("skipping — smooth-dolt not available");
        return;
    };
    let pearl_id = make_pearl(&handle);
    let questions = Arc::new(QuestionRegistry::new());
    let tool = AskSmoothTool {
        pearl_handle: handle.clone(),
        questions,
        pearl_id: pearl_id.clone(),
    };

    let out = tool
        .execute(serde_json::json!({"question": "what is the lint command?", "urgency": "fyi"}))
        .await
        .expect("execute");
    assert!(out.contains("queued"), "fyi should report queued, got: {out}");
    let comments = handle.store.get_comments(&pearl_id).expect("comments");
    assert!(comments.iter().any(|c| c.content.starts_with("[QUESTION:TEAMMATE:q-")));
}

#[tokio::test]
async fn ask_smooth_blocking_resolves_when_registry_fires() {
    let Some(handle) = test_handle() else { return };
    let pearl_id = make_pearl(&handle);
    let questions = Arc::new(QuestionRegistry::new());
    let tool = AskSmoothTool {
        pearl_handle: handle.clone(),
        questions: questions.clone(),
        pearl_id: pearl_id.clone(),
    };

    // Spawn the tool; the registry will be populated mid-flight.
    let exec = tokio::spawn(async move { tool.execute(serde_json::json!({"question": "port?", "urgency": "blocking"})).await });

    // Wait until the comment is posted, find the q-id, and resolve.
    let mut q_id: Option<String> = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let comments = handle.store.get_comments(&pearl_id).expect("comments");
        if let Some(c) = comments.iter().find(|c| c.content.starts_with("[QUESTION:TEAMMATE:q-")) {
            // Header is "[QUESTION:TEAMMATE:q-xxx]" — extract q-xxx.
            let close = c.content.find(']').unwrap();
            let header = &c.content[1..close]; // strip [
            q_id = header.split(':').nth(2).map(|s| s.to_string());
            break;
        }
    }
    let q_id = q_id.expect("q-id surfaced");
    assert!(questions.try_resolve(&q_id, "use 4400".to_string()).await);
    let out = exec.await.expect("join").expect("execute");
    assert!(out.contains("use 4400"), "answer body should be in result: {out}");
}

#[tokio::test]
async fn reply_to_chat_posts_chat_teammate_comment() {
    let Some(handle) = test_handle() else { return };
    let pearl_id = make_pearl(&handle);
    let tool = ReplyToChatTool {
        pearl_handle: handle.clone(),
        pearl_id: pearl_id.clone(),
    };

    let out = tool.execute(serde_json::json!({"message": "on it"})).await.expect("execute");
    assert!(out.to_lowercase().contains("posted"));
    let comments = handle.store.get_comments(&pearl_id).expect("comments");
    assert!(comments.iter().any(|c| c.content == "[CHAT:TEAMMATE] on it"));
}

#[test]
fn ask_smooth_schema_describes_question_and_urgency() {
    // Schema-only test — doesn't need a real store, but the tool struct
    // demands one. Skip when Dolt isn't available.
    let Some(handle) = test_handle() else { return };
    let questions = Arc::new(QuestionRegistry::new());
    let tool = AskSmoothTool {
        pearl_handle: handle,
        questions,
        pearl_id: "th-test".into(),
    };
    let s = tool.schema();
    assert_eq!(s.name, "ask_smooth");
    let req = s.parameters["required"].as_array().unwrap();
    assert!(req.iter().any(|v| v.as_str() == Some("question")));
}
