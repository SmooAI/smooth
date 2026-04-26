---
"@smooai/smooth": patch
---

Fix `/api/projects` and `/api/projects/pearls` hanging on Big Smooth when `smooth-dolt` is on slower storage. Both handlers were calling `PearlStore::open` + `store.stats()` / `store.list()` directly inside `async fn` bodies — those functions shell out to the `smooth-dolt` Go binary via blocking `std::process::Command::output`, pinning the tokio worker for the whole subprocess+IPC roundtrip. With multiple registered projects we did N×subprocess sequentially on a single worker, easily blowing past the request timeout (observed: 60s+ on smoo-hub, never returned). Wrapped both handlers in `tokio::task::spawn_blocking` so the work runs on the blocking thread pool and the runtime stays responsive.
