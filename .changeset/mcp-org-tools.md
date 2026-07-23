---
'@smooai/smooth': minor
---

`th mcp serve` grows a second tier — **talk to your business** from any MCP host.
Local tools stay free (`pearls_ready`/`pearls_create`, plus new `remember`/`recall`
notes). New org tools gate behind Sign in with Smoo (`th auth login`):

- **`ask_business`** — one turn of the Smooth Operator org agent (the same
  user-only `POST /organizations/{org}/smooth-operator/chat` the
  `th api smooth-operator` CLI drives). Auto-resolves the active org, threads
  conversations, and **never sends or takes a destructive action without explicit
  approval** — it returns the paused action + a `conversation_id` you approve by
  calling again with `approve=true`.
- **`knowledge_search`** — a fast semantic read of the org knowledge base
  (verified live end-to-end against prod).

Read-only tools carry MCP `read_only_hint` annotations; the server instructions
teach hosts the free-vs-org tiers and the `th auth login` unlock. Also adds the
`.mcpb` Desktop Extension packaging under `packaging/mcpb/` (one-click Claude
Desktop install; same config drops into Cursor/Windsurf/VS Code).
