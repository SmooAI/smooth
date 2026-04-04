//! # Smooth Coding
//!
//! AI-assisted coding TUI built with ratatui. Provides an interactive
//! chat interface for working with the Smooth Operator agent framework.
//!
//! Entry point: [`app::run`] — call from smooth-cli's `Code` command.

pub mod app;
pub mod layout;
pub mod render;
pub mod state;
pub mod theme;
