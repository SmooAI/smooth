---
'@smooai/smooth': minor
---

th-19dac1: `th harness enable|status|disable` — one idempotent setup/update command per coding harness (Claude Code, Codex, OpenCode), modeled on the TSX-toolbox `tsx agents enable` pattern. `enable` registers the `th mcp serve` MCP server (reusing the preserving per-format writers), installs/updates the smooth-agent plugin via the `claude` CLI where one exists, links the shared skills into `~/.opencode/skills/` for OpenCode, and checks the statusline; `status` verifies each harness (MCP ok/stale/missing via dry-run classification, plugin version, skill links); `disable` removes only what smooth wrote — the MCP entry and symlinks that resolve into smooth-owned sources, never user config. Also: `pnpm install:th` now passes `--locked` to cargo install (an unlocked install re-resolved smooth-operator-core past the lockfile and broke the build), and `smoo --version` answers as the binary instead of erroring on the namespace subcommand.
