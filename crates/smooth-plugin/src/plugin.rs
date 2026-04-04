use async_trait::async_trait;

use crate::command::PluginCommand;

/// The core Plugin trait. Implement this to extend Smooth.
///
/// Plugins can register CLI subcommands, API routes, and smooth-operator tools.
/// Each plugin has a unique identifier, a human-readable name, and a version string.
/// Lifecycle hooks (`init` / `shutdown`) are called once at startup and shutdown.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Unique plugin identifier (e.g., "smooai", "jira", "linear")
    fn id(&self) -> &str;

    /// Human-readable name
    fn name(&self) -> &str;

    /// Version string
    fn version(&self) -> &str;

    /// Called once at startup.
    ///
    /// # Errors
    /// Returns an error if initialization fails.
    async fn init(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called on shutdown.
    ///
    /// # Errors
    /// Returns an error if shutdown cleanup fails.
    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Register CLI subcommands (returns command definitions).
    fn commands(&self) -> Vec<PluginCommand> {
        vec![]
    }

    /// Register API routes (returns axum Router to be nested).
    fn routes(&self) -> Option<axum::Router> {
        None
    }

    /// Register smooth-operator tools.
    fn tools(&self) -> Vec<Box<dyn smooth_operator::tool::Tool>> {
        vec![]
    }
}
