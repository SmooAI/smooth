---
'@smooai/smooth': minor
---

SmoothFlow is now an MCP server (th-1efb59). `th mcp serve` gains `flow_*` tools, so Claude Desktop, Claude Code, Codex or Cursor can list the fleet, start a Claude Code, Codex or shell session in any repo, send a prompt and wait for the turn, answer approvals (with your consent), fan out, and close work out. `th mcp install --harness claude-desktop` registers the server with Claude Desktop on macOS, Windows or Linux. The config entry uses the absolute path to `th`, because Claude Desktop starts with a minimal PATH. The daemon gains HTTP fan-out routes (`POST /api/flow/fanout`, `/fanout/{id}/pick`). `th flow` and the new tools now find the flow engine the way the hooks do, via `flow.addr` first, so they see the same fleet the SmoothFlow app shows.
