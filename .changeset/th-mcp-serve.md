---
'@smooai/smooth': minor
---

`th mcp serve` — run `th` itself as a stdio MCP server, exposing its surfaces as
MCP tools so Claude Desktop / Cursor / Windsurf / VS Code can drive them (the
inverse of the existing `th mcp` client-manager commands). Built on the `rmcp`
SDK; speaks JSON-RPC on stdout. Spike surface is the local, no-login pearls
tools (`pearls_ready`, `pearls_create`); this is the load-bearing tool layer for
the "th as a lead magnet into Claude Desktop" epic (th-63e572) — the same layer
will back a hosted `mcp.smoo.ai` HTTP connector and a one-click `.mcpb` bundle.
