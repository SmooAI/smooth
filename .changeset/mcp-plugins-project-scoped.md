---
"@smooai/smooth": minor
---

MCP servers, CLI-wrapper plugins, and project-scoped config.

- `th mcp add/list/remove/test/path` for stdio MCP servers (Playwright,
  GitHub, filesystem, etc.). Servers register as `<server>.<tool>`.
- `th plugin init/list/path/remove` for file-based CLI-wrapper tools at
  `.smooth/plugins/<name>/plugin.toml`. Plugins register as
  `plugin.<name>` and run the configured shell command with
  `{{placeholder}}` substitution.
- Both MCP and plugins resolve from `~/.smooth/` (global) and
  `<repo>/.smooth/` (project); project entries shadow global on
  name collision. `--project` flags on `add` / `init` / `remove` /
  `path` scope to the repo.
- No trust gate on loading configs — Narc screens every tool call
  (CliGuard, injection, secrets, LLM judge) and the microVM contains
  the blast radius. See `SECURITY.md` and `docs/extending.md`.
