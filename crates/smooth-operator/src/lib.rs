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
pub mod knowledge;
pub mod llm;
pub mod memory;
pub mod tool;

pub use agent::{Agent, AgentConfig, AgentEvent};
pub use checkpoint::{Checkpoint, CheckpointStore, MemoryCheckpointStore};
pub use conversation::{CompactionResult, CompactionStrategy, Conversation, Message, Role};
pub use knowledge::{Document, DocumentType, InMemoryKnowledge, KnowledgeBase, KnowledgeResult};
pub use llm::{accumulate_stream_events, LlmClient, LlmConfig, LlmResponse, StreamEvent};
pub use memory::{InMemoryMemory, Memory, MemoryEntry, MemoryType};
pub use tool::{Tool, ToolCall, ToolRegistry, ToolResult};
