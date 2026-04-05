//! # Smooth Operator
//!
//! Rust-native AI agent framework with built-in checkpointing, tool system,
//! and LLM client. Replaces OpenCode inside Smooth operator microVMs.
//!
//! Inspired by LangGraph, CrewAI, and Agno — purpose-built for orchestrated
//! agent workloads with security-first design.

pub mod agent;
pub mod bigsmooth_client;
pub mod checkpoint;
pub mod conversation;
pub mod cost;
pub mod human;
pub mod knowledge;
pub mod llm;
pub mod memory;
pub mod providers;
pub mod tool;
pub mod workflow;

pub use agent::{Agent, AgentConfig, AgentEvent, DelegationHandle, DelegationTool, SubAgentConfig};
pub use bigsmooth_client::{BigSmoothReporter, ControlEvent, ReporterEvent};
pub use checkpoint::{Checkpoint, CheckpointStore, MemoryCheckpointStore};
pub use conversation::{CompactionResult, CompactionStrategy, Conversation, Message, Role};
pub use cost::{BudgetExceeded, CostBudget, CostEntry, CostTracker, ModelPricing};
pub use human::{human_channel, ConfirmationHook, HumanChannelPair, HumanRequest, HumanResponse};
pub use knowledge::{Document, DocumentType, InMemoryKnowledge, KnowledgeBase, KnowledgeResult};
pub use llm::{accumulate_stream_events, LlmClient, LlmConfig, LlmResponse, StreamEvent};
pub use memory::{InMemoryMemory, Memory, MemoryEntry, MemoryType};
pub use providers::{Activity, ModelRouting, ModelSlot, ProviderConfig, ProviderRegistry};
pub use tool::{Tool, ToolCall, ToolRegistry, ToolResult};
pub use workflow::{EdgeTarget, FnNode, Node, State, Workflow, WorkflowBuilder};
