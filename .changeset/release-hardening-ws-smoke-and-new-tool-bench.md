---
'@smooai/smooth': patch
---

Release-hardening: add an LLM-driven WS smoke suite that drives the real Big
Smooth agent over its canonical WebSocket
(`crates/smooth-daemon/tests/e2e_ws_smoke.rs`) — asserting a plain turn, an
unprompted tool call in Bypass, the full Plan → present_plan → accept → execute
flow, and that a credential read is still blocked. Its event-parsing and
assertion helpers are pure and unit-tested (run in CI with no creds); the four
live flows skip cleanly without `SMOOTH_AGENT_E2E=1` + `SMOOAI_GATEWAY_KEY`. Run
with `pnpm test:e2e:daemon`. Also adds a bench suite for the new personal-agent
tools (`crates/smooth-bench/new-tools-scenarios.toml`: `get_weather`,
`get_location`, `present_plan`) and exposes `permission_hook_with_approver` so the
smoke test installs the exact DenyPolicy-backed gate the daemon runs.
