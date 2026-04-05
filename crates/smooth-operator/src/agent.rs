use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::bigsmooth_client::{BigSmoothReporter, ControlEvent, ReporterEvent};
use crate::checkpoint::{Checkpoint, CheckpointEvent, CheckpointStore, CheckpointStrategy};
use crate::cost::{CostBudget, CostTracker, ModelPricing};
use crate::human::{HumanRequest, HumanResponse};
use crate::knowledge::KnowledgeBase;
use crate::memory::Memory;
use futures_util::StreamExt;

use crate::conversation::{CompactionStrategy, Conversation, Message, ReactiveCompaction};
use crate::llm::{accumulate_stream_events, LlmClient, LlmConfig, StreamEvent};
use crate::tool::{Tool, ToolRegistry, ToolSchema};

/// Configuration for an agent.
#[allow(missing_debug_implementations)]
pub struct AgentConfig {
    pub name: String,
    pub system_prompt: String,
    pub llm: LlmConfig,
    pub max_iterations: u32,
    pub max_context_tokens: usize,
    pub checkpoint_strategy: CheckpointStrategy,
    pub compaction_strategy: CompactionStrategy,
    pub parallel_tools: bool,
    pub memory: Option<Arc<dyn Memory>>,
    pub knowledge: Option<Arc<dyn KnowledgeBase>>,
    pub budget: Option<CostBudget>,
    pub human_tx: Option<UnboundedSender<HumanRequest>>,
    pub human_rx: Option<Arc<tokio::sync::Mutex<UnboundedReceiver<HumanResponse>>>>,
    /// Optional reporter for communicating back to Big Smooth from inside a sandbox.
    pub reporter: Option<Arc<tokio::sync::Mutex<BigSmoothReporter>>>,
}

impl AgentConfig {
    pub fn new(name: impl Into<String>, system_prompt: impl Into<String>, llm: LlmConfig) -> Self {
        Self {
            name: name.into(),
            system_prompt: system_prompt.into(),
            llm,
            max_iterations: 50,
            max_context_tokens: 100_000,
            checkpoint_strategy: CheckpointStrategy::default(),
            compaction_strategy: CompactionStrategy::default(),
            parallel_tools: false,
            memory: None,
            knowledge: None,
            budget: None,
            human_tx: None,
            human_rx: None,
            reporter: None,
        }
    }

    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    pub fn with_parallel_tools(mut self, enabled: bool) -> Self {
        self.parallel_tools = enabled;
        self
    }

    pub fn with_checkpoint_strategy(mut self, strategy: CheckpointStrategy) -> Self {
        self.checkpoint_strategy = strategy;
        self
    }

    pub fn with_compaction_strategy(mut self, strategy: CompactionStrategy) -> Self {
        self.compaction_strategy = strategy;
        self
    }

    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_knowledge(mut self, knowledge: Arc<dyn KnowledgeBase>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    pub fn with_budget(mut self, budget: CostBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    pub fn with_human_channel(mut self, tx: UnboundedSender<HumanRequest>, rx: Arc<tokio::sync::Mutex<UnboundedReceiver<HumanResponse>>>) -> Self {
        self.human_tx = Some(tx);
        self.human_rx = Some(rx);
        self
    }

    /// Set a `BigSmoothReporter` for reporting progress back to Big Smooth.
    pub fn with_reporter(mut self, reporter: Arc<tokio::sync::Mutex<BigSmoothReporter>>) -> Self {
        self.reporter = Some(reporter);
        self
    }
}

/// Events emitted during agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    Started {
        agent_id: String,
    },
    LlmRequest {
        iteration: u32,
        message_count: usize,
    },
    LlmResponse {
        iteration: u32,
        content_preview: String,
        tool_call_count: usize,
    },
    ToolCallStart {
        iteration: u32,
        tool_name: String,
    },
    ToolCallComplete {
        iteration: u32,
        tool_name: String,
        is_error: bool,
    },
    CheckpointSaved {
        checkpoint_id: String,
        iteration: u32,
    },
    Completed {
        agent_id: String,
        iterations: u32,
    },
    MaxIterationsReached {
        agent_id: String,
        max: u32,
    },
    BudgetExceeded {
        spent_usd: f64,
        limit_usd: f64,
    },
    HumanInputRequired {
        request: HumanRequest,
    },
    HumanInputReceived {
        response: HumanResponse,
    },
    Error {
        message: String,
    },
    TokenDelta {
        content: String,
    },
    StreamingComplete,
    DelegationStarted {
        parent_id: String,
        child_id: String,
        task: String,
    },
    DelegationCompleted {
        parent_id: String,
        child_id: String,
        success: bool,
    },
}

/// Configuration for a sub-agent spawned via delegation.
#[derive(Debug, Clone)]
pub struct SubAgentConfig {
    /// System prompt for the sub-agent.
    pub system_prompt: String,
    /// Maximum iterations for the sub-agent's run loop.
    pub max_iterations: u32,
    /// Whether to clone the parent's tools into the sub-agent.
    pub inherit_tools: bool,
}

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: "You are a sub-agent.".into(),
            max_iterations: 10,
            inherit_tools: true,
        }
    }
}

/// Handle to a delegated sub-agent task running in a background tokio task.
pub struct DelegationHandle {
    /// Unique ID of the sub-agent.
    pub agent_id: String,
    /// The task description given to the sub-agent.
    pub task: String,
    join_handle: tokio::task::JoinHandle<anyhow::Result<Conversation>>,
}

impl DelegationHandle {
    /// Wait for the sub-agent to finish and return its conversation.
    ///
    /// # Errors
    /// Returns error if the sub-agent panicked or returned an error.
    pub async fn wait(self) -> anyhow::Result<Conversation> {
        self.join_handle.await.map_err(|e| anyhow::anyhow!("sub-agent task panicked: {e}"))?
    }

    /// Cancel the sub-agent task.
    pub fn cancel(self) {
        self.join_handle.abort();
    }

    /// Check whether the sub-agent task has finished (completed, failed, or cancelled).
    pub fn is_finished(&self) -> bool {
        self.join_handle.is_finished()
    }
}

/// Built-in tool that delegates a task to a sub-agent.
///
/// When called with `{"task": "..."}`, spawns a sub-agent, waits for it
/// to complete, and returns the last assistant message as the tool result.
pub struct DelegationTool {
    agent: Arc<Agent>,
}

impl DelegationTool {
    /// Create a new `DelegationTool` backed by the given parent agent.
    pub fn new(agent: Arc<Agent>) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl Tool for DelegationTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delegate".into(),
            description: "Delegate a task to a sub-agent that will work on it independently and return the result.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The task to delegate to the sub-agent"
                    }
                },
                "required": ["task"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> anyhow::Result<String> {
        let task = arguments
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required 'task' parameter"))?
            .to_string();

        let handle = self.agent.spawn_sub_agent(task, &SubAgentConfig::default());
        let conversation = handle.wait().await?;

        // Return the last assistant message as the result
        let last_assistant = conversation
            .last_assistant_content()
            .unwrap_or("Sub-agent completed with no response.")
            .to_string();

        Ok(last_assistant)
    }
}

/// An AI agent that runs an observe → think → act loop.
pub struct Agent {
    pub id: String,
    config: AgentConfig,
    tools: ToolRegistry,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    event_handler: Option<Box<dyn Fn(AgentEvent) + Send + Sync>>,
    reactive_compaction: std::sync::Mutex<ReactiveCompaction>,
    pub cost_tracker: Arc<Mutex<CostTracker>>,
}

impl Agent {
    pub fn new(config: AgentConfig, tools: ToolRegistry) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            tools,
            checkpoint_store: None,
            event_handler: None,
            reactive_compaction: std::sync::Mutex::new(ReactiveCompaction::new()),
            cost_tracker: Arc::new(Mutex::new(CostTracker::default())),
        }
    }

    pub fn with_checkpoint_store(mut self, store: Arc<dyn CheckpointStore>) -> Self {
        self.checkpoint_store = Some(store);
        self
    }

    pub fn with_event_handler(mut self, handler: impl Fn(AgentEvent) + Send + Sync + 'static) -> Self {
        self.event_handler = Some(Box::new(handler));
        self
    }

    /// Spawn a sub-agent to work on the given task in a background tokio task.
    ///
    /// The sub-agent inherits the parent's `LlmConfig`. If `sub_config.inherit_tools`
    /// is true, the parent's tool registry is cloned (tool `Arc`s are shared, hooks are not).
    pub fn spawn_sub_agent(self: &Arc<Self>, task: String, sub_config: &SubAgentConfig) -> DelegationHandle {
        let tools = if sub_config.inherit_tools {
            self.tools.clone_tools()
        } else {
            ToolRegistry::new()
        };

        let child_config = AgentConfig::new(format!("{}-sub", self.config.name), &sub_config.system_prompt, self.config.llm.clone())
            .with_max_iterations(sub_config.max_iterations);

        let child = Self::new(child_config, tools);
        let child_id = child.id.clone();

        self.emit(AgentEvent::DelegationStarted {
            parent_id: self.id.clone(),
            child_id: child_id.clone(),
            task: task.clone(),
        });

        let task_for_spawn = task.clone();
        let join_handle = tokio::spawn(async move { child.run(&task_for_spawn).await });

        DelegationHandle {
            agent_id: child_id,
            task,
            join_handle,
        }
    }

    /// Resume from the latest checkpoint, or start fresh.
    ///
    /// # Errors
    /// Returns error if checkpoint loading fails.
    pub fn resume_or_new(&self) -> anyhow::Result<Conversation> {
        if let Some(store) = &self.checkpoint_store {
            if let Some(checkpoint) = store.load_latest(&self.id)? {
                tracing::info!(agent_id = %self.id, checkpoint_id = %checkpoint.id, iteration = checkpoint.iteration, "resuming from checkpoint");
                return Ok(checkpoint.conversation);
            }
        }
        Ok(Conversation::new(self.config.max_context_tokens).with_system_prompt(&self.config.system_prompt))
    }

    /// Run the agent loop with a user message.
    ///
    /// # Errors
    /// Returns error if the LLM call or tool execution fails fatally.
    #[allow(clippy::too_many_lines)]
    pub async fn run(&self, user_message: impl Into<String>) -> anyhow::Result<Conversation> {
        let mut conversation = self.resume_or_new()?;
        let user_msg: String = user_message.into();

        // Inject memory/knowledge context before the user message
        let context_messages = self.build_context_messages(&user_msg);
        for msg in context_messages {
            conversation.push(msg);
        }

        conversation.push(Message::user(user_msg));

        self.emit(AgentEvent::Started { agent_id: self.id.clone() });

        let llm = LlmClient::new(self.config.llm.clone());
        let tool_schemas = self.tools.schemas();

        for iteration in 1..=self.config.max_iterations {
            // Check for steering commands from Big Smooth
            if let Some(control) = self.check_steering() {
                match control {
                    ControlEvent::Cancel => {
                        tracing::info!("Received cancel from Big Smooth");
                        self.report_to_bigsmooth(ReporterEvent::TaskError {
                            message: "Cancelled by Big Smooth".into(),
                        });
                        return Ok(conversation);
                    }
                    ControlEvent::Steer { action, message } => {
                        tracing::info!(action, ?message, "Received steering command from Big Smooth");
                        // Inject steering as a system message
                        let steer_msg = format!("[STEERING: {action}] {}", message.unwrap_or_default());
                        conversation.push(Message::system(steer_msg));
                    }
                    _ => {}
                }
            }

            // Compact if approaching context limit
            if conversation.needs_compaction() {
                let result = conversation.compact(&self.config.compaction_strategy, None);
                tracing::info!(
                    messages_removed = result.messages_removed,
                    tokens_before = result.tokens_before,
                    tokens_after = result.tokens_after,
                    "compacted conversation"
                );
            }

            // Observe: get context window
            let context = conversation.context_window();
            let context_refs: Vec<&Message> = context.into_iter().collect();

            self.emit(AgentEvent::LlmRequest {
                iteration,
                message_count: context_refs.len(),
            });

            // Think: call LLM (with reactive compaction on context-length errors)
            let response = match llm.chat(&context_refs, &tool_schemas).await {
                Ok(resp) => resp,
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("prompt_too_long") || err_msg.contains("context_length_exceeded") {
                        // Check circuit breaker before attempting reactive compaction
                        {
                            let rc = self.reactive_compaction.lock().expect("lock reactive_compaction");
                            if rc.is_circuit_open() {
                                return Err(anyhow::anyhow!(
                                    "reactive compaction circuit breaker open after {} consecutive failures: {err_msg}",
                                    rc.stats().consecutive_failures
                                ));
                            }
                        }

                        // Compact the conversation reactively
                        let result = conversation.compact(&self.config.compaction_strategy, None);
                        tracing::warn!(
                            messages_removed = result.messages_removed,
                            tokens_before = result.tokens_before,
                            tokens_after = result.tokens_after,
                            "reactive compaction triggered by context length error"
                        );

                        // Retry with compacted context
                        let retry_context = conversation.context_window();
                        let retry_refs: Vec<&Message> = retry_context.into_iter().collect();
                        match llm.chat(&retry_refs, &tool_schemas).await {
                            Ok(resp) => {
                                self.reactive_compaction.lock().expect("lock reactive_compaction").record_success();
                                resp
                            }
                            Err(retry_err) => {
                                self.reactive_compaction.lock().expect("lock reactive_compaction").record_failure();
                                return Err(retry_err);
                            }
                        }
                    } else {
                        return Err(e);
                    }
                }
            };

            let content_preview = response.content.chars().take(100).collect::<String>();
            self.emit(AgentEvent::LlmResponse {
                iteration,
                content_preview,
                tool_call_count: response.tool_calls.len(),
            });

            // Record cost and check budget
            if self.record_cost_and_check_budget(&response) {
                return Ok(conversation);
            }

            // If LLM returned content, add it as assistant message
            if !response.content.is_empty() || !response.tool_calls.is_empty() {
                let mut msg = Message::assistant(&response.content);
                msg.tool_calls.clone_from(&response.tool_calls);
                conversation.push(msg);
            }

            // Maybe checkpoint after LLM response
            self.maybe_checkpoint(&conversation, iteration, CheckpointEvent::LlmResponse);

            // Act: execute tool calls
            if response.tool_calls.is_empty() {
                // No tool calls = agent is done thinking
                self.emit(AgentEvent::Completed {
                    agent_id: self.id.clone(),
                    iterations: iteration,
                });
                let cost = self.cost_tracker.lock().expect("lock cost_tracker").total_cost_usd;
                self.report_to_bigsmooth(ReporterEvent::TaskComplete {
                    iterations: iteration,
                    cost_usd: cost,
                });
                return Ok(conversation);
            }

            if self.config.parallel_tools {
                for tool_call in &response.tool_calls {
                    self.emit(AgentEvent::ToolCallStart {
                        iteration,
                        tool_name: tool_call.name.clone(),
                    });
                    self.report_to_bigsmooth(ReporterEvent::ToolCallStart {
                        tool_name: tool_call.name.clone(),
                        arguments: tool_call.arguments.to_string(),
                    });
                }

                let results = self.tools.execute_parallel(&response.tool_calls).await;

                for (tool_call, result) in response.tool_calls.iter().zip(&results) {
                    self.emit(AgentEvent::ToolCallComplete {
                        iteration,
                        tool_name: tool_call.name.clone(),
                        is_error: result.is_error,
                    });
                    self.report_to_bigsmooth(ReporterEvent::ToolCallComplete {
                        tool_name: tool_call.name.clone(),
                        result: result.content.chars().take(500).collect(),
                        is_error: result.is_error,
                        duration_ms: 0,
                    });
                    conversation.push(Message::tool_result(&tool_call.id, &result.content));
                    self.maybe_checkpoint(&conversation, iteration, CheckpointEvent::ToolCallComplete);
                }
            } else {
                for tool_call in &response.tool_calls {
                    self.emit(AgentEvent::ToolCallStart {
                        iteration,
                        tool_name: tool_call.name.clone(),
                    });
                    self.report_to_bigsmooth(ReporterEvent::ToolCallStart {
                        tool_name: tool_call.name.clone(),
                        arguments: tool_call.arguments.to_string(),
                    });

                    let start = std::time::Instant::now();
                    let result = self.tools.execute(tool_call).await;
                    let duration_ms = start.elapsed().as_millis() as u64;

                    self.emit(AgentEvent::ToolCallComplete {
                        iteration,
                        tool_name: tool_call.name.clone(),
                        is_error: result.is_error,
                    });
                    self.report_to_bigsmooth(ReporterEvent::ToolCallComplete {
                        tool_name: tool_call.name.clone(),
                        result: result.content.chars().take(500).collect(),
                        is_error: result.is_error,
                        duration_ms,
                    });

                    conversation.push(Message::tool_result(&tool_call.id, &result.content));

                    // Maybe checkpoint after each tool call
                    self.maybe_checkpoint(&conversation, iteration, CheckpointEvent::ToolCallComplete);
                }
            }
        }

        self.emit(AgentEvent::MaxIterationsReached {
            agent_id: self.id.clone(),
            max: self.config.max_iterations,
        });

        Ok(conversation)
    }

    /// Run the agent loop with streaming LLM responses, sending events through a channel.
    ///
    /// This is the streaming counterpart to `run()`. Instead of using the closure-based
    /// event handler, all events (including token deltas) are sent through the provided
    /// `mpsc::UnboundedSender`. This is designed for TUI consumption.
    ///
    /// # Errors
    /// Returns error if the LLM call or tool execution fails fatally.
    #[allow(clippy::too_many_lines)]
    pub async fn run_with_channel(&self, user_message: impl Into<String>, tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>) -> anyhow::Result<Conversation> {
        let mut conversation = self.resume_or_new()?;
        let user_msg: String = user_message.into();

        // Inject memory/knowledge context before the user message
        let context_messages = self.build_context_messages(&user_msg);
        for msg in context_messages {
            conversation.push(msg);
        }

        conversation.push(Message::user(user_msg));

        let _ = tx.send(AgentEvent::Started { agent_id: self.id.clone() });

        let llm = LlmClient::new(self.config.llm.clone());
        let tool_schemas = self.tools.schemas();

        for iteration in 1..=self.config.max_iterations {
            // Compact if approaching context limit
            if conversation.needs_compaction() {
                let result = conversation.compact(&self.config.compaction_strategy, None);
                tracing::info!(
                    messages_removed = result.messages_removed,
                    tokens_before = result.tokens_before,
                    tokens_after = result.tokens_after,
                    "compacted conversation"
                );
            }

            let context = conversation.context_window();
            let context_refs: Vec<&Message> = context.into_iter().collect();

            let _ = tx.send(AgentEvent::LlmRequest {
                iteration,
                message_count: context_refs.len(),
            });

            // Stream the LLM response (with reactive compaction on context-length errors)
            let mut stream = match llm.chat_stream(&context_refs, &tool_schemas).await {
                Ok(s) => s,
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("prompt_too_long") || err_msg.contains("context_length_exceeded") {
                        {
                            let rc = self.reactive_compaction.lock().expect("lock reactive_compaction");
                            if rc.is_circuit_open() {
                                return Err(anyhow::anyhow!(
                                    "reactive compaction circuit breaker open after {} consecutive failures: {err_msg}",
                                    rc.stats().consecutive_failures
                                ));
                            }
                        }

                        let result = conversation.compact(&self.config.compaction_strategy, None);
                        tracing::warn!(
                            messages_removed = result.messages_removed,
                            tokens_before = result.tokens_before,
                            tokens_after = result.tokens_after,
                            "reactive compaction triggered by context length error (streaming)"
                        );

                        let retry_context = conversation.context_window();
                        let retry_refs: Vec<&Message> = retry_context.into_iter().collect();
                        match llm.chat_stream(&retry_refs, &tool_schemas).await {
                            Ok(s) => {
                                self.reactive_compaction.lock().expect("lock reactive_compaction").record_success();
                                s
                            }
                            Err(retry_err) => {
                                self.reactive_compaction.lock().expect("lock reactive_compaction").record_failure();
                                return Err(retry_err);
                            }
                        }
                    } else {
                        return Err(e);
                    }
                }
            };

            // Forward token deltas through the channel while accumulating
            let (accumulator_tx, accumulator_rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(256);

            // Hard per-iteration wall clock — if a single LLM turn takes longer than
            // this, abort and move on. Guards against provider streams that go into
            // TCP CLOSE_WAIT without producing EOF (observed on Anthropic/Kimi via
            // OpenCode Zen). Applies to BOTH the tap loop and accumulator.
            const ITERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
            // Per-item idle timeout inside the tap loop — same guard, shorter scope.
            const ITEM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

            let tap_tx = tx.clone();
            let tap_loop = async {
                loop {
                    let event_result = match tokio::time::timeout(ITEM_IDLE_TIMEOUT, stream.next()).await {
                        Ok(Some(r)) => r,
                        Ok(None) => break, // stream ended cleanly
                        Err(_) => {
                            // Idle timeout — surface as an error so accumulator fails fast.
                            let _ = accumulator_tx
                                .send(Err(anyhow::anyhow!("LLM stream idle timeout: no event for {ITEM_IDLE_TIMEOUT:?}")))
                                .await;
                            return;
                        }
                    };
                    match &event_result {
                        Ok(StreamEvent::Delta { content }) => {
                            let _ = tap_tx.send(AgentEvent::TokenDelta { content: content.clone() });
                        }
                        Ok(StreamEvent::Reasoning { content }) => {
                            // Surface reasoning tokens as TokenDelta so progress is visible
                            // during long reasoning phases (Kimi K2.5, DeepSeek R1, etc.).
                            // The accumulator drops these from the final response content.
                            let _ = tap_tx.send(AgentEvent::TokenDelta { content: content.clone() });
                        }
                        Ok(StreamEvent::Done { .. }) => {
                            let _ = tap_tx.send(AgentEvent::StreamingComplete);
                        }
                        _ => {}
                    }
                    if accumulator_tx.send(event_result).await.is_err() {
                        break;
                    }
                }
                drop(accumulator_tx);
            };

            let rx_stream = tokio_stream::wrappers::ReceiverStream::new(accumulator_rx);
            let accumulate_fut = accumulate_stream_events(Box::pin(rx_stream));

            // Run tap and accumulate concurrently, under a hard wall-clock cap.
            let (_, accumulated) = match tokio::time::timeout(ITERATION_TIMEOUT, async {
                let (_, acc) = tokio::join!(tap_loop, accumulate_fut);
                acc
            })
            .await
            {
                Ok(result) => ((), result?),
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "LLM iteration timeout: no completion within {ITERATION_TIMEOUT:?} on iteration {iteration}"
                    ));
                }
            };
            let response = accumulated;

            let content_preview = response.content.chars().take(100).collect::<String>();
            let _ = tx.send(AgentEvent::LlmResponse {
                iteration,
                content_preview,
                tool_call_count: response.tool_calls.len(),
            });

            // Record cost and check budget
            if self.record_cost_and_check_budget(&response) {
                return Ok(conversation);
            }

            if !response.content.is_empty() || !response.tool_calls.is_empty() {
                let mut msg = Message::assistant(&response.content);
                msg.tool_calls.clone_from(&response.tool_calls);
                conversation.push(msg);
            }

            self.maybe_checkpoint(&conversation, iteration, CheckpointEvent::LlmResponse);

            if response.tool_calls.is_empty() {
                let _ = tx.send(AgentEvent::Completed {
                    agent_id: self.id.clone(),
                    iterations: iteration,
                });
                return Ok(conversation);
            }

            if self.config.parallel_tools {
                for tool_call in &response.tool_calls {
                    let _ = tx.send(AgentEvent::ToolCallStart {
                        iteration,
                        tool_name: tool_call.name.clone(),
                    });
                }

                let results = self.tools.execute_parallel(&response.tool_calls).await;

                for (tool_call, result) in response.tool_calls.iter().zip(&results) {
                    let _ = tx.send(AgentEvent::ToolCallComplete {
                        iteration,
                        tool_name: tool_call.name.clone(),
                        is_error: result.is_error,
                    });
                    conversation.push(Message::tool_result(&tool_call.id, &result.content));
                    self.maybe_checkpoint(&conversation, iteration, CheckpointEvent::ToolCallComplete);
                }
            } else {
                for tool_call in &response.tool_calls {
                    let _ = tx.send(AgentEvent::ToolCallStart {
                        iteration,
                        tool_name: tool_call.name.clone(),
                    });

                    let result = self.tools.execute(tool_call).await;

                    let _ = tx.send(AgentEvent::ToolCallComplete {
                        iteration,
                        tool_name: tool_call.name.clone(),
                        is_error: result.is_error,
                    });

                    conversation.push(Message::tool_result(&tool_call.id, &result.content));
                    self.maybe_checkpoint(&conversation, iteration, CheckpointEvent::ToolCallComplete);
                }
            }
        }

        let _ = tx.send(AgentEvent::MaxIterationsReached {
            agent_id: self.id.clone(),
            max: self.config.max_iterations,
        });

        Ok(conversation)
    }

    /// Build context injection messages from memory and knowledge based on the last user message.
    fn build_context_messages(&self, last_user_message: &str) -> Vec<Message> {
        use std::fmt::Write;
        let mut context_parts = Vec::new();

        if let Some(memory) = &self.config.memory {
            match memory.recall(last_user_message, 5) {
                Ok(entries) if !entries.is_empty() => {
                    let mut buf = String::from("[Recalled memories]\n");
                    for entry in &entries {
                        let _ = writeln!(buf, "- ({:?}, relevance={:.2}): {}", entry.memory_type, entry.relevance, entry.content);
                    }
                    context_parts.push(buf);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to recall memories");
                }
                _ => {}
            }
        }

        if let Some(knowledge) = &self.config.knowledge {
            match knowledge.query(last_user_message, 3) {
                Ok(results) if !results.is_empty() => {
                    let mut buf = String::from("[Relevant knowledge]\n");
                    for result in &results {
                        let _ = writeln!(buf, "- (source={}, score={:.2}): {}", result.source, result.score, result.chunk);
                    }
                    context_parts.push(buf);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to query knowledge base");
                }
                _ => {}
            }
        }

        context_parts.into_iter().map(Message::system).collect()
    }

    fn maybe_checkpoint(&self, conversation: &Conversation, iteration: u32, event: CheckpointEvent) {
        if !self.config.checkpoint_strategy.should_checkpoint(iteration, event) {
            return;
        }

        if let Some(store) = &self.checkpoint_store {
            let checkpoint = Checkpoint::new(&self.id, conversation, iteration);
            let checkpoint_id = checkpoint.id.clone();
            match store.save(&checkpoint) {
                Ok(()) => {
                    self.emit(AgentEvent::CheckpointSaved { checkpoint_id, iteration });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to save checkpoint");
                }
            }
        }
    }

    /// Record cost for an LLM response and check budget. Returns `true` if budget was exceeded.
    fn record_cost_and_check_budget(&self, response: &crate::llm::LlmResponse) -> bool {
        let model = &self.config.llm.model;
        let pricing = ModelPricing::for_model(model);

        {
            let mut tracker = self.cost_tracker.lock().expect("lock cost_tracker");
            tracker.record(model, &response.usage, &pricing);

            if let Some(budget) = &self.config.budget {
                if let Err(exceeded) = tracker.check_budget(budget) {
                    self.emit(AgentEvent::BudgetExceeded {
                        spent_usd: exceeded.spent_usd,
                        limit_usd: exceeded.limit_usd.unwrap_or(0.0),
                    });
                    return true;
                }
            }
        }

        false
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(handler) = &self.event_handler {
            handler(event);
        }
    }

    /// Report an event to Big Smooth via the reporter (if configured). Fire-and-forget.
    fn report_to_bigsmooth(&self, event: ReporterEvent) {
        if let Some(reporter) = &self.config.reporter {
            let reporter = Arc::clone(reporter);
            tokio::spawn(async move {
                let guard = reporter.lock().await;
                if let Err(e) = guard.report(event).await {
                    tracing::warn!(error = %e, "failed to report to Big Smooth");
                }
            });
        }
    }

    /// Check for steering commands from Big Smooth. Returns Some(ControlEvent) if one is pending.
    fn check_steering(&self) -> Option<ControlEvent> {
        if let Some(reporter) = &self.config.reporter {
            // Use try_lock to avoid blocking the agent loop
            if let Ok(mut guard) = reporter.try_lock() {
                return guard.try_recv_control();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::MemoryCheckpointStore;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_config() -> AgentConfig {
        AgentConfig::new("test-agent", "You are a test agent", LlmConfig::opencode_zen("fake-key"))
    }

    #[test]
    fn agent_config_builder() {
        let config = test_config().with_max_iterations(10).with_checkpoint_strategy(CheckpointStrategy::Never);
        assert_eq!(config.max_iterations, 10);
    }

    #[test]
    fn agent_config_parallel_tools() {
        let config = test_config().with_parallel_tools(true);
        assert!(config.parallel_tools);

        let config = test_config();
        assert!(!config.parallel_tools);
    }

    #[test]
    fn agent_creation() {
        let agent = Agent::new(test_config(), ToolRegistry::new());
        assert!(!agent.id.is_empty());
    }

    #[test]
    fn agent_resume_no_checkpoint() {
        let agent = Agent::new(test_config(), ToolRegistry::new());
        let conv = agent.resume_or_new().expect("resume");
        assert_eq!(conv.len(), 1); // system prompt only
    }

    #[test]
    fn agent_resume_with_checkpoint() {
        let store = Arc::new(MemoryCheckpointStore::new());
        let store_dyn: Arc<dyn CheckpointStore> = Arc::clone(&store) as Arc<dyn CheckpointStore>;
        let agent = Agent::new(test_config(), ToolRegistry::new()).with_checkpoint_store(store_dyn);

        // Save a checkpoint with some messages
        let mut conv = Conversation::new(100_000).with_system_prompt("test");
        conv.push(Message::user("previous message"));
        conv.push(Message::assistant("previous response"));
        let cp = Checkpoint::new(&agent.id, &conv, 5);
        store.save(&cp).expect("save");

        // Resume should restore the conversation
        let restored = agent.resume_or_new().expect("resume");
        assert_eq!(restored.len(), 3); // system + user + assistant
    }

    #[test]
    fn event_handler_receives_events() {
        let count = Arc::new(AtomicU32::new(0));
        let count_clone = Arc::clone(&count);

        let _agent = Agent::new(test_config(), ToolRegistry::new()).with_event_handler(move |_event| {
            count_clone.fetch_add(1, Ordering::Relaxed);
        });

        // Events are emitted during run(), which requires async + real LLM
        // Just verify the handler is set up correctly
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn agent_event_serialization() {
        let event = AgentEvent::LlmResponse {
            iteration: 3,
            content_preview: "Hello".into(),
            tool_call_count: 2,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("LlmResponse"));
        assert!(json.contains("\"iteration\":3"));
    }

    #[test]
    fn agent_event_variants() {
        let events = vec![
            AgentEvent::Started { agent_id: "a".into() },
            AgentEvent::LlmRequest {
                iteration: 1,
                message_count: 5,
            },
            AgentEvent::ToolCallStart {
                iteration: 1,
                tool_name: "echo".into(),
            },
            AgentEvent::ToolCallComplete {
                iteration: 1,
                tool_name: "echo".into(),
                is_error: false,
            },
            AgentEvent::CheckpointSaved {
                checkpoint_id: "cp".into(),
                iteration: 1,
            },
            AgentEvent::Completed {
                agent_id: "a".into(),
                iterations: 5,
            },
            AgentEvent::MaxIterationsReached { agent_id: "a".into(), max: 50 },
            AgentEvent::BudgetExceeded {
                spent_usd: 5.0,
                limit_usd: 3.0,
            },
            AgentEvent::Error { message: "oops".into() },
            AgentEvent::TokenDelta { content: "hello".into() },
            AgentEvent::StreamingComplete,
            AgentEvent::DelegationStarted {
                parent_id: "p".into(),
                child_id: "c".into(),
                task: "do something".into(),
            },
            AgentEvent::DelegationCompleted {
                parent_id: "p".into(),
                child_id: "c".into(),
                success: true,
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).expect("serialize");
            assert!(!json.is_empty());
        }
    }

    #[test]
    fn token_delta_event_serialization() {
        let event = AgentEvent::TokenDelta {
            content: "streaming text".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("TokenDelta"));
        assert!(json.contains("streaming text"));
    }

    #[test]
    fn streaming_complete_event_serialization() {
        let event = AgentEvent::StreamingComplete;
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("StreamingComplete"));
    }

    // --- Delegation tests ---

    #[tokio::test]
    async fn delegation_handle_is_finished_lifecycle() {
        // Spawn a trivial background task that completes immediately
        let handle = DelegationHandle {
            agent_id: "child-1".into(),
            task: "say hello".into(),
            join_handle: tokio::spawn(async {
                let conv = Conversation::new(100_000).with_system_prompt("test");
                Ok(conv)
            }),
        };

        // Wait for it to finish
        let conv = handle.wait().await.expect("should complete");
        assert_eq!(conv.len(), 1); // system prompt only
    }

    #[test]
    fn sub_agent_config_defaults() {
        let config = SubAgentConfig::default();
        assert_eq!(config.system_prompt, "You are a sub-agent.");
        assert_eq!(config.max_iterations, 10);
        assert!(config.inherit_tools);
    }

    #[tokio::test]
    async fn spawn_sub_agent_creates_unique_id() {
        let parent = Arc::new(Agent::new(test_config(), ToolRegistry::new()));
        let handle1 = parent.spawn_sub_agent("task 1".into(), &SubAgentConfig::default());
        let handle2 = parent.spawn_sub_agent("task 2".into(), &SubAgentConfig::default());

        assert_ne!(handle1.agent_id, handle2.agent_id);
        assert_ne!(handle1.agent_id, parent.id);

        // Clean up
        handle1.cancel();
        handle2.cancel();
    }

    #[tokio::test]
    async fn delegation_started_event_has_correct_ids() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let parent = Arc::new(Agent::new(test_config(), ToolRegistry::new()).with_event_handler(move |event| {
            events_clone.lock().expect("lock").push(event);
        }));

        let parent_id = parent.id.clone();
        let handle = parent.spawn_sub_agent("test task".into(), &SubAgentConfig::default());
        let child_id = handle.agent_id.clone();
        handle.cancel();

        let events = events.lock().expect("lock");
        let started = events.iter().find(|e| matches!(e, AgentEvent::DelegationStarted { .. }));
        assert!(started.is_some(), "DelegationStarted event should be emitted");

        if let Some(AgentEvent::DelegationStarted {
            parent_id: pid,
            child_id: cid,
            task,
        }) = started
        {
            assert_eq!(pid, &parent_id);
            assert_eq!(cid, &child_id);
            assert_eq!(task, "test task");
        }
    }

    #[test]
    fn agent_config_with_reporter_builder() {
        let reporter = Arc::new(tokio::sync::Mutex::new(BigSmoothReporter::new()));
        let config = test_config().with_reporter(reporter);
        assert!(config.reporter.is_some());

        // Default should be None
        let config2 = test_config();
        assert!(config2.reporter.is_none());
    }

    #[test]
    fn delegation_completed_event_serialization() {
        let event = AgentEvent::DelegationCompleted {
            parent_id: "parent-123".into(),
            child_id: "child-456".into(),
            success: true,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("DelegationCompleted"));
        assert!(json.contains("parent-123"));
        assert!(json.contains("child-456"));
        assert!(json.contains("true"));
    }

    #[tokio::test]
    async fn cancel_aborts_the_task() {
        let handle = DelegationHandle {
            agent_id: "child-abort".into(),
            task: "long task".into(),
            join_handle: tokio::spawn(async {
                // Simulate a long-running task
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok(Conversation::new(100_000))
            }),
        };

        assert!(!handle.is_finished());
        handle.cancel();

        // Give the runtime a moment to propagate the abort
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    #[test]
    fn delegation_tool_schema_has_task_parameter() {
        use crate::tool::Tool;

        let parent = Arc::new(Agent::new(test_config(), ToolRegistry::new()));
        let tool = DelegationTool::new(parent);
        let schema = tool.schema();

        assert_eq!(schema.name, "delegate");
        let params = &schema.parameters;
        assert!(params["properties"]["task"].is_object());
        assert_eq!(params["properties"]["task"]["type"], "string");
        let required = params["required"].as_array().expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("task")));
    }
}
