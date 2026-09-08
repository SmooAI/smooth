---
'@smooai/smooth': minor
---

`th pkg` (EPIC th-55b2c7, M0): install agent packages — skills, rules, MCP
servers, hooks — into Claude Code, Codex and OpenCode from ONE
Claude-plugin-layout source (a path, `owner/repo[/subdir][#ref]`, or a
marketplace.json). A package is one plugin with N harness renderings: the
Claude plugin layout is the shared core, `harness/<name>/` overlays hold what
only that harness understands, and the output is each harness's native shape
(Claude gets it through its own plugin system; Codex/OpenCode get symlinked
skills, MCP entries and their overlay files). Every written path + sha256 and
every owned config key lands in `~/.smooth/pkg/index.toml`, so `th pkg rm`
removes exactly what was installed and `th pkg status` reports drift and the
customization points each harness has. `th harness enable codex|opencode` is
now sugar for installing the smooth-agent package (its OpenCode lifecycle
plugin moved to `harness/opencode/plugin.js`), and the MCP config writers
accept arbitrary server names. `th pkg init` scaffolds the layout.
