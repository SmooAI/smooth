# Harness Packages — `th pkg`

> EPIC **th-55b2c7**. Source: `crates/smooth-cli/src/pkg.rs` (rendering +
> provenance), `crates/smooth-cli/src/mcp_install.rs` (the preserving MCP
> config writers), `crates/smooth-cli/src/harness.rs` (per-harness extras).
> First package: `claude-plugins/smooth-agent`.

A package is **one plugin with N harness renderings**. It is not a
primitive-by-primitive translation layer: the shared core is the Claude Code
plugin layout verbatim, per-harness overlays hold what only that harness
understands, and the output is each harness's **native** shape. Where a harness
has a plugin system (Claude Code), `th pkg` composes core + overlay and hands
it over; where it doesn't (Codex, OpenCode), it installs files and config keys
and records every one of them in `~/.smooth/pkg/index.toml` so `rm` removes
exactly what was installed and `status` can report drift.

`th pkg` never shells out to `apm` or `opkg` (zero runtime dependencies). The
subset it needs is ~1.2k lines of Rust on top of writers that already existed.

## Layout

```
my-package/
├── .claude-plugin/plugin.json     # name (required), version, description, mcpServers
├── skills/<name>/SKILL.md         # shared core — Claude plugin layout, verbatim
├── commands/*.md
├── agents/*.md
├── hooks/hooks.json               # Claude-native hooks (core = Claude layout)
├── .mcp.json                      # {"mcpServers": {name: {command, args, env}}}
├── rules/*.md                     # optional `paths:` frontmatter
└── harness/                       # per-harness overlays — never inside the composed plugin
    ├── claude-code/               # mirrors the plugin root; files REPLACE core (hooks/hooks.json, …)
    ├── codex/config.toml          # fragment key-merged into ~/.codex/config.toml
    ├── opencode/plugin.js         # OpenCode lifecycle plugin
    └── cursor/                    # M1
```

`th pkg init [dir]` scaffolds exactly this. `${CLAUDE_PLUGIN_ROOT}` in MCP
commands is substituted with the cached package root for Codex/OpenCode
(Claude substitutes it itself).

## What M0 renders, per harness

| Harness         | Rendering                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **claude-code** | Handed to Claude's plugin system. GitHub source whose repo ships a `marketplace.json` listing the plugin → `extraKnownMarketplaces.<mk>` (github) + `enabledPlugins.<pkg>@<mk>` in `~/.claude/settings.json` (the shape smooai's `.claude/settings.json` uses). Otherwise core + `harness/claude-code/` are composed into the local `th-pkg` marketplace at `~/.smooth/pkg/claude-marketplace/plugins/<pkg>/` (a `directory` source). Skills, commands, agents, hooks and MCP then load natively. `rules/*.md` → `~/.claude/rules/<pkg>/`. If `<pkg>@<anything>` is already enabled, nothing is registered twice. |
| **codex**       | `skills/*` → `~/.codex/skills/` (symlink; copy on non-unix). `.mcp.json` + `plugin.json.mcpServers` → `[mcp_servers.<name>]` in `~/.codex/config.toml`. `harness/codex/config.toml` deep-merged into `~/.codex/config.toml` (comments and layout survive; every leaf key is recorded as owned).                                                                                                                                                                                                                                                                                                                   |
| **opencode**    | `skills/*` → `~/.opencode/skills/`. MCP → `mcp.<name>` in `~/.config/opencode/opencode.json`. `harness/opencode/plugin.js` → `~/.config/opencode/plugins/<pkg>.js` (symlink).                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| **(all)**       | `skills/*` → `~/.smooth/skills/` so `th` itself (`th skills`, the daemon) discovers them.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |

A harness whose marker dir (`~/.claude`, `~/.codex`, `~/.config/opencode`) is
missing is skipped with a note — nothing is conjured into existence.

Not rendered in M0 (see roadmap): commands/agents for Codex and OpenCode,
hooks key-merge (overlay files replace), rules for Cursor, `--project` scope.

## Sources

```bash
th pkg install ./claude-plugins/smooth-agent            # local plugin dir → copied to cache/<name>@local
th pkg install ./some-repo                              # dir with .claude-plugin/marketplace.json → every listed plugin
th pkg install SmooAI/smooth/claude-plugins/smooth-agent#v1.2   # git clone --depth 1 (apm grammar)
th pkg install https://github.com/SmooAI/smooth         # same, URL spelling
th pkg install https://x.dev/marketplace.json           # remote marketplace: github/owner-repo plugin sources only
```

Sources are cached at `~/.smooth/pkg/cache/<name>@<ref|local|HEAD>`; every
rendering links into that copy, so a source checkout can move or vanish
without breaking a harness. Reinstalling (`install` again) first removes the
previous rendering — a package that dropped a skill leaves no stale link.

## Provenance and safety rules

`~/.smooth/pkg/index.toml`:

```toml
[packages.smooth-agent]
version = "0.41.5"
source = "path:/…/claude-plugins/smooth-agent"
installed_at = "2026-09-08T16:10:09Z"
root = "/Users/me/.smooth/pkg/cache/smooth-agent@local"
harnesses = ["claude-code", "codex", "opencode"]
claude_plugin = "smooth-agent@th-pkg"
notes = ["claude-code: handed to Claude's plugin system as smooth-agent@th-pkg …"]

[[packages.smooth-agent.files]]
harness = "codex"
path = "/Users/me/.codex/skills/th-mail"
kind = "symlink"          # symlink (sha256 of target) | file (sha256 of content) | dir
sha256 = "…"

[[packages.smooth-agent.keys]]
harness = "codex"
file = "/Users/me/.codex/config.toml"
key = ["mcp_servers", "smooth"]   # one segment per element — segments may contain dots
```

- **Never clobber.** A real file or directory where a link would go is left
  alone and noted. A malformed JSON/TOML config errors instead of being
  replaced.
- **`rm` removes only what the index says we own.** Symlinks are removed if
  still symlinks; copied files only if their hash still matches (a
  user-modified rule stays, with a warning); owned keys are removed and
  empty parent tables/objects pruned. A marketplace registration that
  pre-existed is not ours.
- **`status` reports drift**: missing artifacts, links pointing elsewhere,
  hash mismatches, missing keys — plus which customization points
  (`claude-code/hooks`, `codex/config.toml`, `opencode/plugin.js`, `cursor`,
  `rules`) the package provides.
- **Hooks are never translated.** Claude blocks only on exit 2, Copilot uses
  JSON, Codex/Cursor have none — so hooks stay per-harness customization
  points, shipped as explicit shims (`harness/opencode/plugin.js` is one).

## `th harness enable` is now sugar

`th harness enable codex|opencode` runs `th pkg install <smooth-agent
checkout> --harness <x>` (the checkout is the newest Claude plugin cache or
the marketplace clone) plus the MCP entry and the harness's extras.
`claude-code` keeps its `claude plugin install` path and the statusline
check. `th harness disable` still removes the MCP entry and smooth-owned links
(now recognising the `th pkg` cache as smooth-owned); `th pkg rm smooth-agent`
removes the package from every harness at once.

## Roadmap (pearl th-55b2c7)

- **M1 — overlays:** `hooks.json` key-merge instead of replace, settings
  fragments for Claude, rules for Cursor (frontmatter overrides), an `AGENTS.md`
  managed section (apm-style markers), `--project` scope.
- **M2 — commands/agents** for Codex (`~/.codex/prompts`) and OpenCode
  (`command/`, `agent/`), `th pkg pack`, `th pkg update`, richer drift repair.
- **M3 — hosted marketplace:** `smoo pkg publish` serving a per-org
  `marketplace.json`; `ext_trust` content-hash pinning.

Stolen deliberately from the references (`~/dev/refs/apm`, `~/dev/refs/OpenPackage`):
deployed-file hashes, tracked-key deep merge. Skipped deliberately: DSLs, SBOM,
40-platform tables, registries, semver ranges, lockfiles beyond `index.toml`.
