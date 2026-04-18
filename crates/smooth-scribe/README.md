<div align="center">

# smooth-scribe

**Scribe — per-VM structured-logging service**

*Every move the agent makes gets written down. Every tool call, every HTTP request, every escalation. Durable receipts, delivered to the Archivist.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-scribe)](https://crates.io/crates/smooai-smooth-scribe)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

Scribe runs inside every Smooth Operator microVM. It takes structured `LogEntry` records from the agent's audit hook, stores them locally so the operator has full replay even if the network drops, and batch-forwards to a central `Archivist` in the Boardroom for cross-VM correlation.

The point isn't logs. The point is an unbroken audit trail: when a tool call blocks, when a secret gets redacted, when a policy escalates — it's in Scribe first, in the Archivist seconds later, and never gets un-written.

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## Key Types

- **`LogEntry`** — timestamp, level, service, operator_id, message, arbitrary structured fields.
- **`LogLevel`** — `Debug` / `Info` / `Warn` / `Error`.
- **`LogStore`** trait + **`MemoryLogStore`** — query by service, operator, level, time window. Plug your own for persistence.
- **`AuditHook`** — implements `ToolHook` so every pre/post tool call lands in Scribe as a paired `LogEntry` automatically. No agent-side instrumentation required.
- **`ForwarderHandle`** — async batch-forwarder to an `Archivist` URL. Back-pressure handling, retry-with-jitter, at-least-once delivery semantics.

## Usage

```rust
use smooth_scribe::{spawn_forwarder, LogEntry, LogLevel, MemoryLogStore};
use std::sync::Arc;

let store = Arc::new(MemoryLogStore::new());
let _forwarder = spawn_forwarder("http://archivist.internal:8700", store.clone());

store.insert(
    LogEntry::new("operator", LogLevel::Info, "task start")
        .with_operator("op-abc123"),
);
```

## License

MIT
