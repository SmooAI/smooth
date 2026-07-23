---
'@smooai/smooth': minor
---

Configure Smooth Operator's tools from `th` — and from Claude Desktop / Cursor.

New `th api smooth-operator tools list|enable|disable` plus two MCP tools
(`operator_tools`, `operator_tools_set`) over the per-org operator tool-config
API, so you can ask "what can my operator do?" and "turn off email.send" from
the same chat that drives the operator. Org-admin only (the API enforces it).

Writes are **read-modify-write by design**: the PUT body is authoritative and
the server treats any omitted tool as enabled, so sending a single-entry body
would silently re-enable everything else. `set_operator_tool` re-reads the full
catalog, flips one entry, and sends all of it.
