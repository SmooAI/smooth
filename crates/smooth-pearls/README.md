# smooth-pearls

SQLite-backed work item tracker with dependency graphs and per-field history. Built for AI agent orchestration workflows where every task is a "pearl" with full history, comments, and status tracking. One database per machine (`~/.smooth/pearls.db`) holds every project's pearls, keyed by the project's canonical root — so a pearl created inside a git worktree is the same pearl you see from the main checkout.

## Features

- **Dependency Graph** -- Pearls can block/depend on other pearls; `ready()` = open with no open blockers
- **History** -- every field change is recorded per pearl
- **Memories** -- free-form project notes next to the pearls
- **Jira Integration** -- Bidirectional sync with Jira for external project management
- **Global Registry** -- Track projects across the machine from `~/.smooth/registry.json`

## Quick Start

```rust
use smooth_pearls::{PearlStore, PearlQuery, PearlStatus, NewPearl, Priority, PearlType};
use std::path::Path;

fn main() -> anyhow::Result<()> {
    // Open the store for the project containing the cwd (creates ~/.smooth/pearls.db on first use)
    let store = PearlStore::open(Path::new("."))?;

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
