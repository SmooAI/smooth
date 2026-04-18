<div align="center">

# smooth-archivist

**Archivist — keeper of the cross-VM record**

*Every Scribe, every operator, every log line. Ingested, deduplicated, queryable. The only place in the cast that sees the whole picture.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-archivist)](https://crates.io/crates/smooai-smooth-archivist)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

Scribes are local — one per operator microVM, written to from the agent's audit hook. Archivist is global. It runs inside the Boardroom and receives batched `LogEntry` uploads from every Scribe on the network, deduplicates them by `(operator_id, timestamp, message_hash)` so retries from flaky networks don't duplicate entries, and exposes:

- **`POST /ingest`** — accept an `IngestBatch` of entries from a per-VM Scribe.
- **`GET /query`** — filter by service, level, operator, time window, source VM. Backed by `ArchiveStore` (memory impl included; plug your own for durability).
- **`GET /stats`** — counts by VM, level, service. Powers the Boardroom status board.
- **`GET /events`** — SSE stream so the `th code` TUI and the web dashboard can tail live across every operator at once.

This is how the security layer pays off: Narc flags a secret leak on operator 3, Archivist correlates it with the same hash on operator 7, and you find out within seconds that two agents independently got the same adversarial instruction from the same upstream source.

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## Key Types

- **`build_router`** / **`AppState`** — axum router for the four endpoints above.
- **`IngestBatch`** / **`IngestResult`** — wire types for `/ingest`.
- **`ArchiveStore`** trait + **`MemoryArchiveStore`** — query surface with filters.
- **`EventArchive`** — in-memory ring + tokio broadcast channel for live tails plus scrollback.

## Usage

```rust
use smooth_archivist::{build_router, AppState};

let state = AppState::new();
let app = build_router(state);
axum::serve(listener, app).await?;
```

## License

MIT
