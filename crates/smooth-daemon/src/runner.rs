//! In-process agent execution.
//!
//! Where the legacy `dispatch_ws_task_direct` spawned the `smooth-operative`
//! binary inside a microVM, the daemon runs the agent **in its own process**
//! via [`Agent::run_with_channel`] — this is the whole point of dropping the
//! VM substrate. [`run_task`] builds the agent, consumes its `AgentEvent`
//! stream on a side task, translates each event to a wire [`ServerEvent`]
//! ([`crate::wire::map_agent_event`]), forwards it to the connected socket, and
//! records it to the durable [`EventStore`] for cursor-resume.
//!
//! Phase 1 runs **text-only** (an empty [`ToolRegistry`]): it proves the
//! token-streaming path end-to-end. Tool wiring (porting `smooth-operative`'s
//! file/bash/grep tools into a reusable lib + the auto-mode permission hooks)
//! is its own pearl and lands behind this same entry point.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use smooth_operator::{Agent, AgentConfig, CostBudget, Message, ToolRegistry};
use tokio::sync::mpsc::UnboundedSender;

use crate::approval::ApprovalCoordinator;
use crate::config::resolve_llm;
use crate::event::{EventKind, EventStore};
use crate::hook::PermissionHook;
use crate::messages::MessageStore;
use crate::permission::{PermissionEngine, PermissionMode};
use crate::wire::{map_agent_event, PriorMessage, ServerEvent};

/// The daemon's baseline system prompt. Later phases layer project context
/// (`AGENTS.md` / `.smooth/CONTEXT.md`), workspace memory, and cast roles on
/// top; Phase 1 keeps it minimal so the spine is easy to reason about.
const DEFAULT_SYSTEM_PROMPT: &str = "You are Smooth, an always-on personal coding agent running on the operator's own machine. \
You have tools to read, search (grep), list, write, and edit files in the workspace, and to run shell commands (bash). \
When a task asks you to inspect, create, modify, or run something, DO IT with your tools rather than guessing or just describing what to do — then briefly confirm what you did. \
Be concise and direct.";

/// Everything needed to run one agent turn.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    /// Unique id for this turn (echoed in every emitted [`ServerEvent`]).
    pub task_id: String,
    /// The session this turn belongs to (conversation identity).
    pub session_id: String,
    /// The user's prompt.
    pub message: String,
    /// Optional per-task model override.
    pub model: Option<String>,
    /// Optional spend cap in USD.
    pub budget: Option<f64>,
    /// Prior turns to replay before this one (session resume).
    pub prior_messages: Vec<PriorMessage>,
    /// Workspace root the agent's filesystem/shell tools are confined to.
    pub workspace: PathBuf,
}

/// Run one agent turn to completion, streaming `ServerEvent`s to `out` and
/// recording them to `events`. Never panics; failures are surfaced as a
/// terminal [`ServerEvent::TaskError`].
pub async fn run_task(
    spec: TaskSpec,
    out: UnboundedSender<ServerEvent>,
    events: Arc<dyn EventStore>,
    messages: Arc<dyn MessageStore>,
    approvals: Arc<ApprovalCoordinator>,
    mode: PermissionMode,
    egress_proxy: Option<String>,
) {
    let TaskSpec {
        task_id,
        session_id,
        message,
        model,
        budget,
        prior_messages,
        workspace,
    } = spec;
    // Saved for durable history; `message` itself is moved into the engine.
    let user_message = message.clone();

    let llm = match resolve_llm(model.as_deref()) {
        Ok(llm) => llm,
        Err(e) => {
            emit(
                &out,
                &events,
                &session_id,
                ServerEvent::TaskError {
                    task_id,
                    message: format!("LLM config error: {e:#}"),
                },
            )
            .await;
            return;
        }
    };

    let mut cfg = AgentConfig::new(format!("daemon-{session_id}"), DEFAULT_SYSTEM_PROMPT, llm);
    if !prior_messages.is_empty() {
        cfg = cfg.with_prior_messages(to_engine_messages(&prior_messages));
    }
    if let Some(max_cost_usd) = budget {
        cfg = cfg.with_budget(CostBudget {
            max_cost_usd: Some(max_cost_usd),
            max_tokens: None,
        });
    }

    // Register the workspace-confined tool set (fs/grep/…) + the Gate-1
    // permission hook (deny→ask→allow, with the operator-approval round-trip).
    // Security note: tools enforce lexical path confinement and the hook gates
    // intent; the kernel OS-sandbox enforcement boundary is Phase 3 Slice 2.
    let mut tools = ToolRegistry::new();
    // When an egress proxy is configured, the bash tool routes network through
    // it (direct off-box egress kernel-denied) — the goalie exact-host allowlist
    // becomes the only way out.
    smooth_tools::register_default_tools_with_proxy(&mut tools, workspace, egress_proxy);
    tools.add_hook(PermissionHook::new(PermissionEngine::new(mode), approvals, out.clone()));
    let agent = Agent::new(cfg, tools);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<smooth_operator::AgentEvent>();

    // Drain the engine's event stream on a side task so the channel never
    // back-pressures the agent loop. The engine emits its own terminal
    // (`Completed` / `MaxIterationsReached`) on the normal paths; we track
    // whether one was forwarded so we can synthesize a terminal for the rare
    // early-return path and for hard errors.
    let saw_terminal = Arc::new(AtomicBool::new(false));
    // Accumulate streamed assistant text for durable conversation history.
    let assistant = Arc::new(Mutex::new(String::new()));
    let consumer = {
        let task_id = task_id.clone();
        let session_id = session_id.clone();
        let out = out.clone();
        let events = Arc::clone(&events);
        let saw_terminal = Arc::clone(&saw_terminal);
        let assistant = Arc::clone(&assistant);
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if let smooth_operator::AgentEvent::TokenDelta { content } = &ev {
                    assistant.lock().unwrap_or_else(PoisonError::into_inner).push_str(content);
                }
                if let Some(se) = map_agent_event(&task_id, &ev) {
                    if is_terminal(&se) {
                        saw_terminal.store(true, Ordering::SeqCst);
                    }
                    emit(&out, &events, &session_id, se).await;
                }
            }
        })
    };

    let result = agent.run_with_channel(message, tx).await;
    // `tx` was moved into the loop and is dropped on return → `rx` closes →
    // the consumer drains and exits. Wait for it before we read/synthesize.
    let _ = consumer.await;

    // Persist this turn to durable history on success, so the conversation
    // resumes after a daemon restart. `prior_messages` already held the
    // earlier turns; we only append the current user + assistant pair.
    if result.is_ok() {
        let _ = messages.append(&session_id, "user", &user_message).await;
        let text = assistant.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if !text.trim().is_empty() {
            let _ = messages.append(&session_id, "assistant", &text).await;
        }
    }

    if saw_terminal.load(Ordering::SeqCst) {
        return;
    }
    let terminal = match result {
        Ok(_conversation) => ServerEvent::TaskComplete {
            task_id,
            iterations: 0,
            cost_usd: 0.0,
        },
        Err(e) => ServerEvent::TaskError {
            task_id,
            message: format!("{e:#}"),
        },
    };
    emit(&out, &events, &session_id, terminal).await;
}

fn is_terminal(se: &ServerEvent) -> bool {
    matches!(se, ServerEvent::TaskComplete { .. } | ServerEvent::TaskError { .. })
}

/// Forward a wire event to the socket and append it to the durable log.
///
/// The durable record uses [`EventKind::Raw`] in Phase 1 (the full
/// `ServerEvent` JSON); Phase 2 maps to typed [`EventKind`] variants as the
/// Dolt-backed store lands. A failed socket send is ignored — the client has
/// gone away and the run will be cancelled by the socket's close handler.
async fn emit(out: &UnboundedSender<ServerEvent>, events: &Arc<dyn EventStore>, session_id: &str, se: ServerEvent) {
    if let Ok(payload) = serde_json::to_value(&se) {
        let _ = events.append(session_id, EventKind::Raw { payload }).await;
    }
    let _ = out.send(se);
}

fn to_engine_messages(prior: &[PriorMessage]) -> Vec<Message> {
    prior
        .iter()
        .map(|m| {
            if m.role == "assistant" {
                Message::assistant(&m.content)
            } else {
                Message::user(&m.content)
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;
    use crate::event::InMemoryEventLog;

    /// With no LLM configured, a task fails fast with a terminal TaskError
    /// (no panic, no hang) — exercisable without a real model.
    #[tokio::test]
    async fn missing_llm_config_yields_terminal_task_error() {
        // Ensure no creds resolve: clear env + point providers.json at nothing.
        std::env::remove_var("SMOOTH_API_URL");
        std::env::remove_var("SMOOTH_API_KEY");
        std::env::set_var("SMOOTH_PROVIDERS_FILE", "/nonexistent/smooth-daemon/providers.json");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let events: Arc<dyn EventStore> = Arc::new(InMemoryEventLog::new());
        let messages: Arc<dyn MessageStore> = Arc::new(crate::messages::InMemoryMessageStore::new());

        run_task(
            TaskSpec {
                task_id: "t1".into(),
                session_id: "s1".into(),
                message: "hello".into(),
                model: Some("some-model".into()),
                budget: None,
                prior_messages: vec![],
                workspace: std::env::temp_dir(),
            },
            tx,
            Arc::clone(&events),
            Arc::clone(&messages),
            crate::approval::ApprovalCoordinator::new(),
            crate::permission::PermissionMode::default(),
            None,
        )
        .await;

        let ev = rx.recv().await.expect("a terminal event");
        match ev {
            ServerEvent::TaskError { task_id, message } => {
                assert_eq!(task_id, "t1");
                assert!(message.contains("config"), "should explain the config problem: {message}");
            }
            other => panic!("expected TaskError, got {other:?}"),
        }
        // And it was recorded durably.
        assert_eq!(events.latest_seq().await.unwrap(), 1);
    }

    #[test]
    fn prior_messages_map_to_roles() {
        let prior = vec![
            PriorMessage {
                role: "user".into(),
                content: "hi".into(),
            },
            PriorMessage {
                role: "assistant".into(),
                content: "hello".into(),
            },
        ];
        let msgs = to_engine_messages(&prior);
        assert_eq!(msgs.len(), 2);
    }
}
