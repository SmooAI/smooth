//! # Smooth Plugin
//!
//! Trait-based plugin system for extending Smooth with CLI commands,
//! API routes, TUI views, and smooth-operator tools.
//!
//! Third-party extensions implement the [`Plugin`] trait to register
//! their functionality, then get loaded into a [`PluginRegistry`]
//! at startup.

pub mod command;
pub mod plugin;
pub mod registry;

pub use command::{PluginCommand, PluginCommandBuilder};
pub use plugin::Plugin;
pub use registry::PluginRegistry;
