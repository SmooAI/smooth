---
'smooth': patch
---

daemon: send temperature 1.0, not 0.0 — 0 is rejected by many frontier models

A growing set of models reject any temperature but their default and 400 the whole
request ("Unsupported value: 'temperature' does not support 0 with this model").
The symptom doesn't look like a config error: the daemon boots, accepts the turn,
every LLM call 400s, and the user sees an assistant that says nothing.

A per-model allowlist would be provably wrong — `gpt-5.1` rejects while `gpt-5.2`
accepts, `gpt-5.4` accepts while `gpt-5.4-pro` rejects. `1.0` was accepted by all
12 models tested across 6 families, so it is the one value that works everywhere.

This covers the daemon's own LLM configs (narc judge, sidekick factory, env-resolved
gateway). The main chat turn's config is built inside the engine and still needs the
same fix there — tracked in th-c127d1.
