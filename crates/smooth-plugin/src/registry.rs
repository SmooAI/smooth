use std::collections::HashMap;
use std::sync::Arc;

use smooth_operator::tool::ToolRegistry;

use crate::plugin::Plugin;

/// Registry that holds all loaded plugins.
///
/// Provides lifecycle management (init/shutdown), plugin lookup, and aggregation
/// of routes and tools from all registered plugins.
pub struct PluginRegistry {
    plugins: HashMap<String, Arc<dyn Plugin>>,
    /// Insertion order for deterministic iteration.
    order: Vec<String>,
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            order: vec![],
        }
    }

    /// Register a plugin. Returns an error if a plugin with the same id already exists.
    ///
    /// # Errors
    /// Returns an error if a plugin with a duplicate id is registered.
    pub fn register(&mut self, plugin: Arc<dyn Plugin>) -> anyhow::Result<()> {
        let id = plugin.id().to_string();
        if self.plugins.contains_key(&id) {
            anyhow::bail!("duplicate plugin id: {id}");
        }
        self.order.push(id.clone());
        self.plugins.insert(id, plugin);
        Ok(())
    }

    /// Get a plugin by id.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Plugin>> {
        self.plugins.get(id)
    }

    /// List all registered plugins in insertion order.
    pub fn list(&self) -> Vec<&Arc<dyn Plugin>> {
        self.order.iter().filter_map(|id| self.plugins.get(id)).collect()
    }

    /// Initialize all plugins in registration order.
    ///
    /// # Errors
    /// Returns the first plugin initialization error encountered.
    pub async fn init_all(&self) -> anyhow::Result<()> {
        for id in &self.order {
            if let Some(plugin) = self.plugins.get(id) {
                plugin.init().await?;
            }
        }
        Ok(())
    }

    /// Shutdown all plugins in reverse registration order.
    ///
    /// # Errors
    /// Returns the first plugin shutdown error encountered.
    pub async fn shutdown_all(&self) -> anyhow::Result<()> {
        for id in self.order.iter().rev() {
            if let Some(plugin) = self.plugins.get(id) {
                plugin.shutdown().await?;
            }
        }
        Ok(())
    }

    /// Collect all plugin routes into a single merged axum Router.
    /// Each plugin's routes are nested under `/{plugin_id}`.
    pub fn collect_routes(&self) -> axum::Router {
        let mut router = axum::Router::new();
        for id in &self.order {
            if let Some(plugin) = self.plugins.get(id) {
                if let Some(plugin_router) = plugin.routes() {
                    router = router.nest(&format!("/{id}"), plugin_router);
                }
            }
        }
        router
    }

    /// Collect all plugin tools into a single `ToolRegistry`.
    pub fn collect_tools(&self) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        for id in &self.order {
            if let Some(plugin) = self.plugins.get(id) {
                for tool in plugin.tools() {
                    registry.register(tool);
                }
            }
        }
        registry
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::routing::get;
    use smooth_operator::tool::{Tool, ToolSchema};

    use super::*;
    use crate::command::PluginCommandBuilder;
    use crate::plugin::Plugin;

    /// A minimal test plugin with configurable behavior.
    struct TestPlugin {
        id: String,
        name: String,
        version: String,
        provide_routes: bool,
        provide_tools: bool,
        fail_init: bool,
        fail_shutdown: bool,
    }

    impl TestPlugin {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                name: format!("Test Plugin {id}"),
                version: "1.0.0".to_string(),
                provide_routes: false,
                provide_tools: false,
                fail_init: false,
                fail_shutdown: false,
            }
        }

        fn with_routes(mut self) -> Self {
            self.provide_routes = true;
            self
        }

        fn with_tools(mut self) -> Self {
            self.provide_tools = true;
            self
        }

        fn with_fail_init(mut self) -> Self {
            self.fail_init = true;
            self
        }

        fn with_fail_shutdown(mut self) -> Self {
            self.fail_shutdown = true;
            self
        }
    }

    #[async_trait]
    impl Plugin for TestPlugin {
        fn id(&self) -> &str {
            &self.id
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            &self.version
        }

        async fn init(&self) -> anyhow::Result<()> {
            if self.fail_init {
                anyhow::bail!("init failed for {}", self.id);
            }
            Ok(())
        }

        async fn shutdown(&self) -> anyhow::Result<()> {
            if self.fail_shutdown {
                anyhow::bail!("shutdown failed for {}", self.id);
            }
            Ok(())
        }

        fn commands(&self) -> Vec<crate::command::PluginCommand> {
            vec![PluginCommandBuilder::new("test-cmd").description("A test command").build()]
        }

        fn routes(&self) -> Option<axum::Router> {
            if self.provide_routes {
                Some(axum::Router::new().route("/health", get(|| async { "ok" })))
            } else {
                None
            }
        }

        fn tools(&self) -> Vec<Box<dyn Tool>> {
            if self.provide_tools {
                vec![Box::new(DummyTool {
                    name: format!("{}-tool", self.id),
                })]
            } else {
                vec![]
            }
        }
    }

    struct DummyTool {
        name: String,
    }

    #[async_trait]
    impl Tool for DummyTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.clone(),
                description: "A dummy tool".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(&self, _arguments: serde_json::Value) -> anyhow::Result<String> {
            Ok("done".into())
        }
    }

    // --- Plugin trait default implementation tests ---

    struct MinimalPlugin;

    #[async_trait]
    impl Plugin for MinimalPlugin {
        fn id(&self) -> &str {
            "minimal"
        }
        fn name(&self) -> &str {
            "Minimal"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
    }

    #[tokio::test]
    async fn plugin_default_init_succeeds() {
        let p = MinimalPlugin;
        assert!(p.init().await.is_ok());
    }

    #[tokio::test]
    async fn plugin_default_shutdown_succeeds() {
        let p = MinimalPlugin;
        assert!(p.shutdown().await.is_ok());
    }

    #[test]
    fn plugin_default_commands_empty() {
        let p = MinimalPlugin;
        assert!(p.commands().is_empty());
    }

    #[test]
    fn plugin_default_routes_none() {
        let p = MinimalPlugin;
        assert!(p.routes().is_none());
    }

    #[test]
    fn plugin_default_tools_empty() {
        let p = MinimalPlugin;
        assert!(p.tools().is_empty());
    }

    // --- PluginCommand builder tests ---

    #[test]
    fn command_builder_basic() {
        let cmd = PluginCommandBuilder::new("deploy").description("Deploy something").build();
        assert_eq!(cmd.name, "deploy");
        assert_eq!(cmd.description, "Deploy something");
        assert!(cmd.handler.is_none());
        assert!(cmd.subcommands.is_empty());
    }

    #[tokio::test]
    async fn command_builder_with_handler() {
        let cmd = PluginCommandBuilder::new("greet")
            .description("Say hello")
            .handler(|args| async move {
                assert_eq!(args, vec!["world".to_string()]);
                Ok(())
            })
            .build();

        assert!(cmd.handler.is_some());
        let result = (cmd.handler.as_ref().expect("handler should exist"))(vec!["world".into()]).await;
        assert!(result.is_ok());
    }

    #[test]
    fn command_builder_with_subcommands() {
        let sub = PluginCommandBuilder::new("sub").description("A subcommand").build();
        let cmd = PluginCommandBuilder::new("parent").subcommand(sub).build();
        assert_eq!(cmd.subcommands.len(), 1);
        assert_eq!(cmd.subcommands[0].name, "sub");
    }

    // --- PluginRegistry tests ---

    #[test]
    fn registry_register_and_get() {
        let mut reg = PluginRegistry::new();
        let plugin = Arc::new(TestPlugin::new("alpha"));
        reg.register(plugin).expect("register should succeed");

        let found = reg.get("alpha");
        assert!(found.is_some());
        assert_eq!(found.expect("plugin exists").id(), "alpha");
    }

    #[test]
    fn registry_duplicate_id_rejected() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("dup"))).expect("first register ok");
        let result = reg.register(Arc::new(TestPlugin::new("dup")));
        assert!(result.is_err());
        assert!(result.expect_err("should error").to_string().contains("duplicate plugin id"));
    }

    #[test]
    fn registry_list_returns_insertion_order() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("b"))).expect("ok");
        reg.register(Arc::new(TestPlugin::new("a"))).expect("ok");
        reg.register(Arc::new(TestPlugin::new("c"))).expect("ok");

        let ids: Vec<&str> = reg.list().iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
    }

    #[tokio::test]
    async fn registry_init_all_succeeds() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("one"))).expect("ok");
        reg.register(Arc::new(TestPlugin::new("two"))).expect("ok");
        assert!(reg.init_all().await.is_ok());
    }

    #[tokio::test]
    async fn registry_init_all_propagates_error() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("good"))).expect("ok");
        reg.register(Arc::new(TestPlugin::new("bad").with_fail_init())).expect("ok");

        let result = reg.init_all().await;
        assert!(result.is_err());
        assert!(result.expect_err("should fail").to_string().contains("init failed"));
    }

    #[tokio::test]
    async fn registry_shutdown_all_succeeds() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("one"))).expect("ok");
        reg.register(Arc::new(TestPlugin::new("two"))).expect("ok");
        assert!(reg.shutdown_all().await.is_ok());
    }

    #[tokio::test]
    async fn registry_shutdown_all_propagates_error() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("good"))).expect("ok");
        reg.register(Arc::new(TestPlugin::new("bad").with_fail_shutdown())).expect("ok");

        let result = reg.shutdown_all().await;
        assert!(result.is_err());
    }

    #[test]
    fn registry_collect_routes() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("api1").with_routes())).expect("ok");
        reg.register(Arc::new(TestPlugin::new("api2").with_routes())).expect("ok");
        reg.register(Arc::new(TestPlugin::new("no-routes"))).expect("ok");

        // Should not panic — routes are merged successfully
        let _router = reg.collect_routes();
    }

    #[test]
    fn registry_collect_tools() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(TestPlugin::new("t1").with_tools())).expect("ok");
        reg.register(Arc::new(TestPlugin::new("t2").with_tools())).expect("ok");
        reg.register(Arc::new(TestPlugin::new("no-tools"))).expect("ok");

        let tool_reg = reg.collect_tools();
        let schemas = tool_reg.schemas();
        assert_eq!(schemas.len(), 2);

        let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"t1-tool"));
        assert!(names.contains(&"t2-tool"));
    }

    #[test]
    fn registry_get_nonexistent() {
        let reg = PluginRegistry::new();
        assert!(reg.get("missing").is_none());
    }
}
