<div align="center">

# smooth-wonk

**Wonk — per-VM access control authority**

*The rule engine. No LLM, no guesswork, no second-guessing. Wonk decides. Goalie enforces. Narc escalates. Wonk says yes or Wonk says no.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-wonk)](https://crates.io/crates/smooai-smooth-wonk)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

Wonk is the security authority inside every Smooth Operator microVM. It holds the policy TOML, watches it for hot-reload via `notify`, swaps it atomically via `ArcSwap`, and answers four questions on HTTP:

- `POST /check/network` — is this domain + path + method allowed? (Goalie calls this on every outbound request.)
- `POST /check/tool` — is this tool name allowed in the current phase? (the agent's tool-registry hook calls this before dispatch.)
- `POST /check/mcp` — is this MCP server / resource allowed?
- `POST /check/port` — can this port be exposed for forwarding?

Every endpoint requires `Authorization: Bearer <operator_token>` from the per-VM policy's `[auth]` section. Constant-time comparison. Localhost origin is not enough — we expect Goalie and the runner to carry the token; stray binaries installed via `apk add` cannot.

When the rule engine returns "unknown", Wonk escalates to the Boardroom Narc's LLM-judge via `NarcClient`, caches the decision by policy hash, and serves the answer. No per-call latency tax for common decisions.

```
agent tool ─▶ WonkHook ─▶ /check/tool ─▶ decision
operator HTTP ─▶ Goalie ─▶ /check/network ─▶ decision
runner ─▶ Negotiator ─▶ /request ─▶ access negotiation
```

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## Key Types

- **`build_router`** / **`run_server`** — axum surface.
- **`PolicyHolder`** — hot-reloadable policy container backed by `ArcSwap` + `notify`.
- **`Negotiator`** / **`AccessRequest`** / **`AccessResponse`** — runtime access negotiation with Big Smooth (for requests a policy doesn't predict).
- **`WonkHook`** — `ToolHook` impl that wires Wonk decisions into the agent's tool registry.
- **`NarcClient`** — LLM-judge escalation to the Boardroom's central Narc.

## Usage

```rust
use smooth_wonk::{build_router, AppState, PolicyHolder, Negotiator};
use std::sync::Arc;

let policy = PolicyHolder::from_toml(include_str!("policy.toml"))?;
let negotiator = Negotiator::new("http://big-smooth.boardroom:4400", policy.clone());
let state = Arc::new(AppState::new(policy, negotiator));
let app = build_router(state);
axum::serve(listener, app).await?;
```

## License

MIT
