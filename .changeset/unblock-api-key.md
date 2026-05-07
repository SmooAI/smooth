---
"@smooai/smooth": patch
---

sandbox: pass `SMOOTH_API_KEY` as a plain env var (interim — secret substitution silently broken)

Sandboxed dispatch wired `SMOOTH_API_KEY` through microsandbox's
`SecretBuilder` with a placeholder + `allowed_hosts`, expecting the
network layer to swap on outbound. Confirmed in
`smooth-bootstrap-bill/server.rs` — `n.secret(...).env(...).value(...).
placeholder(...).allow_host(...)`. But the runner's single-agent path
makes a Bearer-auth request to `https://llm.smoo.ai/v1` and the literal
`SMOOTH_PLACEHOLDER_API_KEY_NOT_SUBSTITUTED` reaches LiteLLM, which
returns 401: "Authentication Error, LiteLLM Virtual Key expected.
Received=SMOO****UTED, expected to start with 'sk-'".

Likely cause: microsandbox 0.3.14's `NetworkPolicy::allow_all()`
(set when `allow_loopback=true`, which is the default for our
sandbox config) bypasses the secret-substitution middleware. The
two compose oddly. May be fixed in 0.4.x.

Until that's investigated (parent pearl `th-6030b0`), bigsmooth
injects the real API key directly via `env.insert("SMOOTH_API_KEY",
api_key)` and passes an empty `secrets: Vec::new()` to the sandbox
config. Known regression: agents in the VM can read their own
LLM API key (exfil risk via tool output, scraped logs, etc.). The
runner still sends the same Bearer auth — LiteLLM now sees the
real `sk-` key and accepts.
