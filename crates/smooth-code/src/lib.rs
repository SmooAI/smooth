//! # Smooth Coding
//!
//! AI-assisted coding TUI built with ratatui. Provides an interactive
//! chat interface for working with the Smooth Operator agent framework.
//!
//! Entry point: [`app::run`] — call from smooth-cli's `Code` command.

pub mod app;
pub mod autocomplete;
pub mod commands;
pub mod files;
pub mod git;
pub mod layout;
pub mod model_picker;
pub mod permissions;
pub mod render;
pub mod session;
pub mod state;
pub mod theme;
