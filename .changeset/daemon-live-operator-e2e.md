---
'smooai-smooth-daemon': minor
---

EPIC th-c89c2a: integration + live-LLM E2E for the operator local flavor
(`tests/live_operator_e2e.rs`). Boots the local flavor in-process the way
`serve_local_flavor` does (LocalTokenVerifier + the exposed `local_tool_provider`
+ a real gateway config) and drives the canonical WS protocol with a real
client. The gated test (`SMOOTH_AGENT_E2E=1` + `SMOOAI_GATEWAY_KEY`) runs a real
LLM turn that makes the agent call the kernel-sandboxed `bash` tool and round-trips
its output back — proving protocol + agent loop + sandboxed tool execution + the
live gateway end-to-end. The always-on test documents a finding: the operator's
`/ws` degrades a missing/invalid token to an **anonymous** connection rather than
rejecting it, so `LocalTokenVerifier` does NOT gate connections (only ACL scope) —
the loopback bind is the real gate today. Module docs corrected accordingly.
