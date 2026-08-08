---
'@smooai/smooth': patch
---

Fix three first-run blockers on the golden-path onboarding flow (pearl th-6062ea).

- **The daemon never read `providers.json`.** It looked for a provider with the
  hardcoded id `"smooth"`, but every current writer of `~/.smooth/providers.json`
  stamps `"smooai-gateway"` (or `ollama`/`openai`/… for BYO). Resolution now
  follows the routing slot's own `provider` id, falling back to the `coding`
  slot's provider and then the sole provider, so BYO providers work through the
  same path instead of only the gateway.
- **`th model login smooai-gateway` panicked** — the recommended default in the
  picker had no arm in the config-builder match. The picker list and the builder
  are now derived from one catalog, and a test fails the build if an entry ever
  loses its constructor again.
- **19 stale hints pointed at `th auth login`** for a missing LLM provider.
  `th auth login` has been identity-only since 2026-05 and takes no provider
  argument; those hints now say `th model login`.
