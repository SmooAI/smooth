# smooth-pearls

Dolt-backed work item tracker with dependency graphs, version control, and git sync. Built for AI agent orchestration workflows where every task is a "pearl" with full history, comments, and status tracking.

## Features

- **Dependency Graph** -- Pearls can block/depend on other pearls with cycle detection
- **Version Control** -- Backed by embedded Dolt for full commit history and branching
- **Git Sync** -- Push/pull pearl data to Dolt remotes for team collaboration
- **Jira Integration** -- Bidirectional sync with Jira for external project management
- **Session Messages** -- Store conversation history and orchestrator snapshots alongside work items
- **Global Registry** -- Track pearl databases across multiple projects from `~/.smooth/`

## Quick Start

```rust
use smooth_pearls::{PearlStore, PearlQuery, PearlStatus, NewPearl, Priority, PearlType};
use std::path::Path;

fn main() -> anyhow::Result<()> {
    // Initialize a pearl store in the current project
    let store = PearlStore::init(Path::new(".smooth/dolt"))?;

    // Create a pearl
    let pearl = store.create(&NewPearl {
        title: "Implement auth middleware".into(),
        description: "Add JWT validation to all API routes".into(),
        pearl_type: PearlType::Task,
        priority: Priority::High,
        labels: vec!["backend".into(), "security".into()],
        jira_key: Some("PROJ-42".into()),
    })?;

    println!("Created pearl: {}", pearl.id);

    // Query open pearls
    let open = store.query(&PearlQuery {
        status: Some(PearlStatus::Open),
        ..Default::default()
    })?;

    for p in &open {
        println!("{} [{}] {}", p.id, p.priority, p.title);
    }

    // Add a dependency
    store.add_dependency(&pearl.id, "th-abc123")?;

    // Close when done
    store.close(&[&pearl.id])?;

    Ok(())
}
```

## License

MIT

## Links

- [GitHub](https://github.com/SmooAI/smooth)
- [crates.io](https://crates.io/crates/smooth-pearls)
