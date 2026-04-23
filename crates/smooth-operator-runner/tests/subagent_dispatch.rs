//! Integration test: `dispatch_subagent` runs a subagent in isolation
//! and only its final summary crosses back into the parent's
//! conversation.
//!
//! The test stands up a tiny OpenAI-compat mock server on a random
//! port and serves canned SSE streams keyed off the system prompt:
//!
//! - Parent (`code` primary agent): first response calls
//!   `dispatch_subagent({agent: "explore", prompt: "..."})`, second
//!   response is a plain final message — i.e. the classic two-turn
//!   parent pattern.
//! - Subagent (`explore`): one response, plain final message, no
//!   tool calls.
//!
//! We then assert that the parent's conversation contains exactly
//! ONE tool-result message from the dispatch, and that the tool
//! result is the compact `{agent, turns, final_message}` JSON —
//! never the subagent's own transcript.

#![allow(clippy::uninlined_format_args)]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde_json::{json, Value};
use smooth_operator::agents::{AgentRegistry, DispatchSubagentTool};
use smooth_operator::conversation::Role;
use smooth_operator::llm::{ApiFormat, LlmConfig, RetryPolicy};
use smooth_operator::providers::Activity;
use smooth_operator::tool::ToolRegistry;
use smooth_operator::{Agent, AgentConfig};

/// Shared state for the mock LLM server. Each incoming request
/// increments the counter so we can serve different responses to
/// successive parent turns without storing per-request cookies.
#[derive(Clone, Default)]
struct MockState {
    parent_turn: Arc<AtomicUsize>,
    explore_turn: Arc<AtomicUsize>,
}

/// Minimal subset of the OpenAI chat-completions request we need to
/// inspect. Only `messages[0]` (system prompt) is load-bearing — we
/// route on its content.
#[derive(Debug, serde::Deserialize)]
struct ChatReqLite {
    messages: Vec<ChatMsgLite>,
}

#[derive(Debug, serde::Deserialize)]
struct ChatMsgLite {
    role: String,
    content: Option<Value>,
}

/// Hand-rolled SSE framing: one `data: {...}` line per chunk,
/// terminated by `data: [DONE]`. Agent's streaming parser
/// (`parse_sse_line`) expects this shape.
fn sse_body(chunks: &[Value]) -> String {
    let mut out = String::new();
    for c in chunks {
        out.push_str("data: ");
        out.push_str(&c.to_string());
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    out
}

/// Serve a stream that issues a single assistant final message. Used
/// for the subagent's one turn and the parent's second turn.
fn final_message_stream(text: &str) -> String {
    let chunks = vec![
        json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "model": "mock",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": text },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "model": "mock",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop",
            }],
        }),
    ];
    sse_body(&chunks)
}

/// Serve a stream that issues a `dispatch_subagent` tool call. Used
/// for the parent's first turn.
fn tool_call_stream(call_id: &str, agent: &str, prompt: &str) -> String {
    let args = json!({"agent": agent, "prompt": prompt}).to_string();
    let chunks = vec![
        // First chunk: tool call start (role + name + id).
        json!({
            "id": "chatcmpl-tc",
            "object": "chat.completion.chunk",
            "model": "mock",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": "dispatch_subagent",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null,
            }],
        }),
        // Second chunk: arguments payload.
        json!({
            "id": "chatcmpl-tc",
            "object": "chat.completion.chunk",
            "model": "mock",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": args }
                    }]
                },
                "finish_reason": null,
            }],
        }),
        // Finish.
        json!({
            "id": "chatcmpl-tc",
            "object": "chat.completion.chunk",
            "model": "mock",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls",
            }],
        }),
    ];
    sse_body(&chunks)
}

/// Sentinel strings we embed in the mock responses so the test can
/// cleanly assert which transcript content did or did not bleed
/// into the parent's conversation.
const EXPLORE_FINAL: &str = "EXPLORE_SUMMARY: found 3 uses of X in src/";
const EXPLORE_INTERNAL: &str = "INTERNAL_EXPLORE_STEP_DO_NOT_LEAK";
const PARENT_FINAL: &str = "PARENT_FINAL_MESSAGE_AFTER_DISPATCH";

async fn chat_handler(State(state): State<MockState>, Json(body): Json<ChatReqLite>) -> impl IntoResponse {
    // Route on the first system message — the agents have
    // distinct system prompts, so one substring check suffices.
    let system_text = body
        .messages
        .iter()
        .find(|m| m.role == "system")
        .and_then(|m| m.content.as_ref())
        .map(|c| match c {
            Value::String(s) => s.clone(),
            // Some clients send content as an array of {type,text}; we don't
            // exercise that path, but flatten defensively.
            other => other.to_string(),
        })
        .unwrap_or_default();

    let is_explore = system_text.to_lowercase().contains("scout");

    let body = if is_explore {
        // Subagent turn. We embed both a "final" sentinel (what the
        // parent should see) and an "internal" sentinel (what must
        // NOT leak). Only the final goes through because the
        // subagent emits the whole string as its single assistant
        // message — the internal sentinel is just a string inside
        // that message which we'll check is not replicated into
        // the PARENT's conversation (the parent sees only the
        // dispatch tool's JSON result, not the subagent's raw
        // message).
        state.explore_turn.fetch_add(1, Ordering::SeqCst);
        let text = format!("{EXPLORE_INTERNAL}\n\n{EXPLORE_FINAL}");
        final_message_stream(&text)
    } else {
        // Parent turn. First time → tool call. Second time → final.
        let turn = state.parent_turn.fetch_add(1, Ordering::SeqCst);
        if turn == 0 {
            tool_call_stream("call-dispatch-1", "explore", "find uses of X")
        } else {
            final_message_stream(PARENT_FINAL)
        }
    };

    (StatusCode::OK, [("content-type", "text/event-stream")], body).into_response()
}

async fn spawn_mock() -> (String, MockState) {
    let state = MockState::default();
    let app = Router::new().route("/chat/completions", post(chat_handler)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("http://{addr}"), state)
}

fn mock_llm_config(api_url: &str) -> LlmConfig {
    LlmConfig {
        api_url: api_url.into(),
        api_key: "test".into(),
        model: "mock".into(),
        max_tokens: 8192,
        temperature: 0.0,
        retry_policy: RetryPolicy::default(),
        api_format: ApiFormat::OpenAiCompat,
    }
}

#[tokio::test]
async fn code_agent_dispatches_explore_and_only_final_summary_leaks() {
    let (api_url, state) = spawn_mock().await;
    let agents = Arc::new(AgentRegistry::builtin());
    let code = agents.get("code").expect("'code' agent registered").clone();

    let api_for_factory = api_url.clone();
    let llm_factory: smooth_operator::LlmConfigFactory =
        Arc::new(move |_activity: Activity| -> anyhow::Result<LlmConfig> { Ok(mock_llm_config(&api_for_factory)) });

    // Parent's tool registry: just the dispatch tool. The parent's
    // `code` agent has full permissions so the PermissionHook won't
    // block the dispatch call. No other tools are registered because
    // the canned parent response only ever calls dispatch.
    let mut parent_tools = ToolRegistry::new();
    let dispatch = DispatchSubagentTool::new(Arc::clone(&agents), ToolRegistry::new(), Arc::clone(&llm_factory)).with_max_iterations(3);
    parent_tools.register(dispatch);
    parent_tools.add_hook(smooth_operator::PermissionHook::new(&code));

    let parent_cfg = AgentConfig::new("parent-code", &code.prompt, mock_llm_config(&api_url)).with_max_iterations(4);
    let parent = Agent::new(parent_cfg, parent_tools);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let conversation = parent.run_with_channel("please investigate", tx).await.expect("parent run");
    let _ = drain.await;

    // The parent's LLM was called exactly twice (turn 1 = tool call,
    // turn 2 = final). The subagent's LLM was called exactly once.
    assert_eq!(state.parent_turn.load(Ordering::SeqCst), 2, "parent LLM turn count");
    assert_eq!(state.explore_turn.load(Ordering::SeqCst), 1, "subagent LLM turn count");

    // Tool-result messages in the parent's conversation. There
    // should be EXACTLY ONE — the dispatch_subagent result. No
    // other tool_result messages from the subagent should leak.
    let tool_results: Vec<_> = conversation.messages.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tool_results.len(),
        1,
        "expected exactly one tool result, got {}: {:?}",
        tool_results.len(),
        tool_results.iter().map(|m| &m.content).collect::<Vec<_>>()
    );

    // The tool result content is the DispatchResult JSON — the
    // compact summary, NOT the subagent's transcript.
    let tool_result_body = &tool_results[0].content;
    let parsed: Value = serde_json::from_str(tool_result_body).unwrap_or_else(|e| panic!("tool result must be JSON, was: {tool_result_body:?} ({e})"));
    assert_eq!(parsed["agent"], "explore");
    assert!(parsed["turns"].as_u64().unwrap_or(0) >= 1, "turns should be >= 1, got {:?}", parsed["turns"]);
    let final_message = parsed["final_message"].as_str().expect("final_message string");
    assert!(
        final_message.contains("EXPLORE_SUMMARY"),
        "final_message must include subagent summary: {final_message}"
    );

    // Exactly three fields — no extra leakage vector.
    let obj = parsed.as_object().expect("tool result is object");
    assert_eq!(obj.len(), 3, "DispatchResult must have exactly 3 fields: {obj:?}");

    // SANITY: none of the parent's messages OUTSIDE the tool result
    // body contain the subagent's raw message. (The tool result
    // body does contain the final_message string, which is fine —
    // that's the summary. But the raw subagent assistant message
    // itself, or any intermediate turn, must not be replicated as
    // a message in the parent's conversation.)
    for msg in &conversation.messages {
        if msg.role == Role::Tool {
            // Allowed — that's the dispatch result. The
            // EXPLORE_INTERNAL sentinel IS embedded in the
            // subagent's final assistant message (which was the
            // sole message the subagent produced), so it will
            // appear inside final_message. That's acceptable
            // because the SUBAGENT chose to include it. What we
            // care about is that no separate Tool / Assistant
            // message in the PARENT was synthesized from
            // subagent transcript content.
            continue;
        }
        // Assistant messages in the parent are either the
        // tool-call turn (no text content that could leak) or the
        // final PARENT_FINAL message. Never the subagent's
        // transcript.
        if msg.role == Role::Assistant {
            let c = &msg.content;
            assert!(
                !c.contains("EXPLORE_SUMMARY") && !c.contains("INTERNAL_EXPLORE_STEP"),
                "parent assistant message leaked subagent transcript: {c}"
            );
        }
    }

    // The parent's FINAL assistant message is the canned parent
    // response, not anything derived from the subagent transcript.
    let final_parent = conversation.last_assistant_content().expect("parent produced a final message");
    assert!(
        final_parent.contains(PARENT_FINAL),
        "parent final message not the canned one, got: {final_parent}"
    );
}

#[tokio::test]
async fn dispatch_unknown_agent_returns_clean_tool_error() {
    // No LLM calls in this test — we invoke the tool directly.
    let agents = Arc::new(AgentRegistry::builtin());
    let llm_factory: smooth_operator::LlmConfigFactory = Arc::new(|_a: Activity| -> anyhow::Result<LlmConfig> { Ok(mock_llm_config("http://127.0.0.1:1")) });
    let dispatch = DispatchSubagentTool::new(Arc::clone(&agents), ToolRegistry::new(), llm_factory);

    // Register it in a parent registry and invoke through the
    // registry to prove the error propagates as a normal tool
    // result (is_error=true) rather than crashing the parent.
    let mut tools = ToolRegistry::new();
    tools.register(dispatch);

    let result = tools
        .execute(&smooth_operator::tool::ToolCall {
            id: "call-1".into(),
            name: "dispatch_subagent".into(),
            arguments: json!({"agent": "nonexistent", "prompt": "do stuff"}),
        })
        .await;

    assert!(result.is_error, "unknown agent should produce is_error tool result");
    assert!(
        result.content.contains("not a dispatchable subagent") && result.content.contains("nonexistent"),
        "unexpected error content: {}",
        result.content
    );
    assert_eq!(result.tool_call_id, "call-1");
}

#[tokio::test]
async fn dispatch_primary_agent_name_returns_clean_tool_error() {
    // Dispatching to 'code' (a primary, not a subagent) must fail
    // with the same clean error — not fall through to spawning a
    // code agent loop.
    let agents = Arc::new(AgentRegistry::builtin());
    let llm_factory: smooth_operator::LlmConfigFactory = Arc::new(|_a: Activity| -> anyhow::Result<LlmConfig> { Ok(mock_llm_config("http://127.0.0.1:1")) });
    let dispatch = DispatchSubagentTool::new(Arc::clone(&agents), ToolRegistry::new(), llm_factory);

    let mut tools = ToolRegistry::new();
    tools.register(dispatch);

    let result = tools
        .execute(&smooth_operator::tool::ToolCall {
            id: "call-2".into(),
            name: "dispatch_subagent".into(),
            arguments: json!({"agent": "code", "prompt": "recurse"}),
        })
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("not a dispatchable subagent"), "error content: {}", result.content);
    assert!(result.content.contains("code"), "error should name 'code': {}", result.content);
}
