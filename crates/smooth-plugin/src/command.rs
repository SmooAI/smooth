use std::future::Future;
use std::pin::Pin;

/// Type alias for the async handler function used by plugin commands.
pub type CommandHandler = Box<dyn Fn(Vec<String>) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

/// A CLI command provided by a plugin.
///
/// Commands have a name, description, optional subcommands, and an optional
/// async handler function. Use [`PluginCommandBuilder`] for ergonomic construction.
pub struct PluginCommand {
    pub name: String,
    pub description: String,
    pub subcommands: Vec<Self>,
    pub handler: Option<CommandHandler>,
}

impl std::fmt::Debug for PluginCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginCommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("subcommands", &self.subcommands)
            .field("handler", &self.handler.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

/// Builder for constructing [`PluginCommand`] instances.
pub struct PluginCommandBuilder {
    name: String,
    description: String,
    subcommands: Vec<PluginCommand>,
    handler: Option<CommandHandler>,
}

impl PluginCommandBuilder {
    /// Create a new builder with the given command name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            subcommands: vec![],
            handler: None,
        }
    }

    /// Set the command description.
    #[must_use]
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Add a subcommand.
    #[must_use]
    pub fn subcommand(mut self, cmd: PluginCommand) -> Self {
        self.subcommands.push(cmd);
        self
    }

    /// Set the async handler function.
    #[must_use]
    pub fn handler<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.handler = Some(Box::new(move |args| Box::pin(f(args))));
        self
    }

    /// Build the `PluginCommand`.
    pub fn build(self) -> PluginCommand {
        PluginCommand {
            name: self.name,
            description: self.description,
            subcommands: self.subcommands,
            handler: self.handler,
        }
    }
}
