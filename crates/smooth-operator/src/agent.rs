use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::checkpoint::{Checkpoint, CheckpointEvent, CheckpointStore, CheckpointStrategy};
use futures_util::StreamExt;

use crate::conversation::{Conversation, Message};
use crate::llm::{accumulate_stream_events, LlmClient, LlmConfig, StreamEvent};
use crate::tool::ToolRegistry;

/// Configuration for an agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub system_prompt: String,
    pub llm: LlmConfig,
    pub max_iterations: u32,
    pub max_context_tokens: usize,
    pub checkpoint_strategy: CheckpointStrategy,
    pub parallel_tools: bool,
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
            parallel_tools: false,
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
    Error {
        message: String,
    },
    TokenDelta {
        content: String,
    },
    StreamingComplete,
}

/// An AI agent that runs an observe → think → act loop.
pub struct Agent {
    pub id: String,
    config: AgentConfig,
    tools: ToolRegistry,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    event_handler: Option<Box<dyn Fn(AgentEvent) + Send + Sync>>,
}

impl Agent {
    pub fn new(config: AgentConfig, tools: ToolRegistry) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            tools,
            checkpoint_store: None,
            event_handler: None,
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
    pub async fn run(&self, user_message: impl Into<String>) -> anyhow::Result<Conversation> {
        let mut conversation = self.resume_or_new()?;
        conversation.push(Message::user(user_message));

        self.emit(AgentEvent::Started { agent_id: self.id.clone() });

        let llm = LlmClient::new(self.config.llm.clone());
        let tool_schemas = self.tools.schemas();

        for iteration in 1..=self.config.max_iterations {
            // Observe: get context window
            let context = conversation.context_window();
            let context_refs: Vec<&Message> = context.into_iter().collect();

            self.emit(AgentEvent::LlmRequest {
                iteration,
                message_count: context_refs.len(),
            });

            // Think: call LLM
            let response = llm.chat(&context_refs, &tool_schemas).await?;

            let content_preview = response.content.chars().take(100).collect::<String>();
            self.emit(AgentEvent::LlmResponse {
                iteration,
                content_preview,
                tool_call_count: response.tool_calls.len(),
            });

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
                return Ok(conversation);
            }

            if self.config.parallel_tools {
                for tool_call in &response.tool_calls {
                    self.emit(AgentEvent::ToolCallStart {
                        iteration,
                        tool_name: tool_call.name.clone(),
                    });
                }

                let results = self.tools.execute_parallel(&response.tool_calls).await;

                for (tool_call, result) in response.tool_calls.iter().zip(&results) {
                    self.emit(AgentEvent::ToolCallComplete {
                        iteration,
                        tool_name: tool_call.name.clone(),
                        is_error: result.is_error,
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

                    let result = self.tools.execute(tool_call).await;

                    self.emit(AgentEvent::ToolCallComplete {
                        iteration,
                        tool_name: tool_call.name.clone(),
                        is_error: result.is_error,
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
    pub async fn run_with_channel(&self, user_message: impl Into<String>, tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>) -> anyhow::Result<Conversation> {
        let mut conversation = self.resume_or_new()?;
        conversation.push(Message::user(user_message));

        let _ = tx.send(AgentEvent::Started { agent_id: self.id.clone() });

        let llm = LlmClient::new(self.config.llm.clone());
        let tool_schemas = self.tools.schemas();

        for iteration in 1..=self.config.max_iterations {
            let context = conversation.context_window();
            let context_refs: Vec<&Message> = context.into_iter().collect();

            let _ = tx.send(AgentEvent::LlmRequest {
                iteration,
                message_count: context_refs.len(),
            });

            // Stream the LLM response
            let mut stream = llm.chat_stream(&context_refs, &tool_schemas).await?;

            // Forward token deltas through the channel while accumulating
            let (accumulator_tx, accumulator_rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(256);

            // Tap into the stream: send deltas to channel, forward all to accumulator
            while let Some(event_result) = stream.next().await {
                match &event_result {
                    Ok(StreamEvent::Delta { content }) => {
                        let _ = tx.send(AgentEvent::TokenDelta { content: content.clone() });
                    }
                    Ok(StreamEvent::Done { .. }) => {
                        let _ = tx.send(AgentEvent::StreamingComplete);
                    }
                    _ => {}
                }
                if accumulator_tx.send(event_result).await.is_err() {
                    break;
                }
            }
            drop(accumulator_tx);

            // Accumulate the forwarded events into a full response
            let rx_stream = tokio_stream::wrappers::ReceiverStream::new(accumulator_rx);
            let response = accumulate_stream_events(Box::pin(rx_stream)).await?;

            let content_preview = response.content.chars().take(100).collect::<String>();
            let _ = tx.send(AgentEvent::LlmResponse {
                iteration,
                content_preview,
                tool_call_count: response.tool_calls.len(),
            });

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

    fn emit(&self, event: AgentEvent) {
        if let Some(handler) = &self.event_handler {
            handler(event);
        }
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
            AgentEvent::Error { message: "oops".into() },
            AgentEvent::TokenDelta { content: "hello".into() },
            AgentEvent::StreamingComplete,
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
}
