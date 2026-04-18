<div align="center">

# smooth-goalie

**Goalie — in-VM network proxy with zero trust**

*Every packet the agent sends goes through Goalie. Every decision goes to Wonk. No decisions happen inside Goalie — it is strictly enforcement and audit. You do not route around the keeper.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-goalie)](https://crates.io/crates/smooai-smooth-goalie)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

Goalie is the network enforcement point inside every Smooth Operator microVM. `iptables` rules redirect **all** outbound TCP to the Goalie proxy; the agent has no socket path that doesn't go through here. For each request, Goalie calls Wonk's `/check/network` endpoint with `{domain, path, method}`, writes a JSON-lines audit entry with the verdict, and then either forwards the request upstream or returns `403 Forbidden` with the policy reason.

No LLM call. No heuristics. Just a rule engine decision, an audit line, and a pass-through — the kind of boring infrastructure a security audit is happy to read.

```
agent tool ──HTTP──▶ Goalie ──/check/network──▶ Wonk
                        │                        │
                        │  ◀───── decision ──────┤
                        │
                        ├──▶ AuditLogger (JSON-lines, rotating)
                        │
                        └── forward upstream  (if allowed)
                            OR return 403     (if denied)
```

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## Key Types

- **`run_proxy`** — starts the proxy on a given address. Graceful shutdown on signal.
- **`WonkClient`** / **`WonkDecision`** — HTTP client to Wonk. `WonkClient::with_auth(url, token)` carries the per-VM operator token so Wonk's bearer-auth middleware accepts the request.
- **`AuditLogger`** / **`AuditEntry`** — rotating JSON-lines audit log. Every entry has `allowed`, `reason`, `domain`, `path`, `method`, `timestamp`. Tail-able live by an operator-side Scribe.

## Usage

```rust
use smooth_goalie::{run_proxy, AuditLogger, WonkClient};

let wonk = WonkClient::with_auth("http://127.0.0.1:4200", "operator-token");
let audit = AuditLogger::new("/var/log/goalie.jsonl")?;
run_proxy("0.0.0.0:3128", wonk, audit).await?;
```

## License

MIT
