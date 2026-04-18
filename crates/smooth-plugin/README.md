<div align="center">

# smooth-plugin

**The Smooth plugin system**

*Extend Smooth with custom CLI commands, API routes, TUI views, and operator tools. One trait, one registry, zero ceremony.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-plugin)](https://crates.io/crates/smooai-smooth-plugin)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

`smooth-plugin` is the extension point for everything Smooth doesn't ship in the core binary. Build a linter, a deployer, a ticket-sync tool, a bespoke MCP bridge — implement the `Plugin` trait, drop it in `~/.smooth/plugins/` (global) or `<repo>/.smooth/plugins/` (per-project), and it shows up as a `th <name>` subcommand + on the agent's tool registry + on the TUI sidebar.

Plugins merge with project-scope winning on name collisions, so you can override a team-wide plugin locally without forking.

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## Key Types

- **`Plugin`** — the trait: `name`, `version`, plus optional `register_commands`, `register_tools`, `register_api_routes`, `register_tui_views`.
- **`PluginCommand` / `PluginCommandBuilder`** — declarative clap-style subcommands mounted under `th`.
- **`PluginRegistry`** — loaded once at startup; merges global + project-scoped plugins.

## Usage

```rust
use smooth_plugin::{Plugin, PluginCommand, PluginCommandBuilder};

pub struct MyPlugin;

impl Plugin for MyPlugin {
    fn name(&self) -> &str { "my-plugin" }
    fn version(&self) -> &str { env!("CARGO_PKG_VERSION") }

    fn register_commands(&self) -> Vec<PluginCommand> {
        vec![
            PluginCommandBuilder::new("greet")
                .about("Say hello from my-plugin")
                .run(|_args| { println!("hello"); Ok(()) })
                .build(),
        ]
    }
}
```

See [`docs/extending.md`](https://github.com/SmooAI/smooth/blob/main/docs/extending.md) for the full authoring guide, including MCP server bridges and dynamic plugin loading.

## License

MIT
