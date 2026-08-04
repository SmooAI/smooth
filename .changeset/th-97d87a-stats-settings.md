---
'@smooai/smooth': patch
---

Big Smooth web: add **Stats** and **Settings** tabs (pearl th-97d87a).

The daemon UI was chat-only. Now the sidebar has three tabs — Chat, Stats, Settings:

- **Stats** — real usage + spend. The gateway (LiteLLM) reports the authoritative per-call cost (`x-litellm-response-cost-*` → the engine's `gateway_cost_usd`, riding `eventual_response.usage.costUsd`), but nothing persisted it, so the client now POSTs each turn to a new `POST /api/usage` route (appended to `~/.smooth/usage.jsonl`); `GET /api/stats` aggregates it (total spend, in/out tokens, by-model, by-day) alongside durable activity counts (conversations, sessions active/ended, messages in/out, last activity) read from the operator db via a new `SqliteStorageAdapter::activity_snapshot`.
- **Settings** — surfaces controls that were previously only reachable via the composer slash-menu/localStorage: the Smooth Mode (per-turn model) picker, notifications enrollment, connection info (daemon URL, token/identity), and a pointer to the macOS access grants on the menu-bar app.

No new dependency (the unused `react-router-dom` stays unused; a `view` state switch fits three tabs). Presence styling throughout — teal for the active/proportion signal, no decorative amber.
