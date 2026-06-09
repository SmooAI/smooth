//! # smooth-cast — the coding-harness bits the generic engine dropped
//!
//! The published `smooai-smooth-operator-core` engine (0.14.0) is a clean,
//! generic agent engine: it ships the agent loop, the tool system, the
//! generic [`Cast`](smooth_operator::cast::Cast) mechanism + generic roles,
//! checkpointing, memory, and the workflow graph — but it deliberately
//! dropped the `th code` coding-harness specifics that only smooth used.
//!
//! This crate re-homes those specifics so smooth keeps working against the
//! published engine:
//!
//! - [`coding_workflow`] — the `th code` single-agent outer loop
//!   (`run_coding_workflow`, `task_text_has_cleanup_intent`, …), built on
//!   the engine's generic `Agent`/`ProviderRegistry`/`ToolRegistry` API.
//! - [`skills`] — skill discovery (`discover`, `SkillScope`,
//!   `SkillSource`, `Skill`) plus the built-in `create-skill` skill.
//! - [`cast`] — the four coding-harness cast roles the engine no longer
//!   ships (`fixer`, `oracle`, `chief`, `intent_classifier`) and a
//!   [`cast::builtin()`] that returns them on top of the engine's generic
//!   built-in roles.

pub mod cast;
pub mod coding_workflow;
pub mod skills;
