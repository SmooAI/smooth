//! # Smooth Operator
//!
//! Rust-native AI agent framework with built-in checkpointing, tool system,
//! and LLM client. Replaces OpenCode inside Smooth operator microVMs.
//!
//! Inspired by LangGraph, CrewAI, and Agno — purpose-built for orchestrated
//! agent workloads with security-first design.

pub mod agent;
pub mod checkpoint;
pub mod conversation;
pub mod llm;
pub mod tool;

pub use agent::{Agent, AgentConfig, AgentEvent};
pub use checkpoint::{Checkpoint, CheckpointStore, MemoryCheckpointStore};
pub use conversation::{Conversation, Message, Role};
pub use llm::{LlmClient, LlmConfig, LlmResponse};
pub use tool::{Tool, ToolCall, ToolRegistry, ToolResult};
