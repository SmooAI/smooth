---
"@smooai/smooth": patch
---

runner: single-agent path now resolves the LLM config from the active role's slot

When the workflow gate skips `coding_workflow` (oracle / mapper /
heckler — anything that isn't a Coding-slot lead with `bash`
allowed), the runner falls through to the single-agent path. That
path was building `LlmConfig` from the `SMOOTH_*` env vars, which
big-smooth populates from the default provider's default model
(`smooth-fast-gemini` in the canonical setup). Result: oracle
(`slot = Reasoning`) was calling the **Fast** model instead of
Reasoning — both wrong-tier-for-the-task and the very model that
just hit a 503 on Vertex AI.

After active-role resolution but before `agent_config` is built,
re-parse the routing JSON (already mounted into the sandbox at
`/opt/smooth/policy/routing.json`), then ask
`ProviderRegistry::llm_config_for(active_role.slot)` for the
right model. That config replaces the env-var default for the
single-agent path. The workflow path is unaffected — it does its
own per-phase resolution further down using the same registry.

Falls back to the env-var default cleanly when the routing JSON
is missing, unparseable, or the slot can't resolve — preserves
existing behavior for tests and minimal setups.
