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
it over; where it doesn't (Codex, OpenCode, Cursor), it installs files, config
keys, hook entries and managed `AGENTS.md` sections and records every one of
them in `~/.smooth/pkg/index.toml` so `rm` removes exactly what was installed
and `status` can report drift.

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
├── rules/*.md                     # optional `paths:` / `description:` frontmatter
└── harness/                       # per-harness overlays — never inside the composed plugin
    ├── claude-code/               # mirrors the plugin root; files replace core, EXCEPT
    │   └── hooks/hooks.json       #   hooks.json, which is key-merged with core (M1)
    ├── codex/config.toml          # fragment key-merged into ~/.codex/config.toml
    ├── codex/hooks.json           # Claude-style hooks key-merged into ~/.codex/hooks.json (M1)
    ├── opencode/plugin.js         # OpenCode lifecycle plugin
    └── cursor/rules/<stem>.mdc    # replaces the .mdc rendered from rules/<stem>.md (M1)
```

`th pkg init [dir]` scaffolds exactly this. `${CLAUDE_PLUGIN_ROOT}` in MCP
commands and in the Codex hooks overlay is substituted with the cached package
root (Claude substitutes it itself; Codex spells its own variable
`${PLUGIN_ROOT}` and only for plugins it loads, so a `~/.codex/hooks.json`
entry needs the absolute path).

## What `install` renders, per harness

| Harness         | Rendering                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **claude-code** | Handed to Claude's plugin system. GitHub source whose repo ships a `marketplace.json` listing the plugin → `extraKnownMarketplaces.<mk>` (github) + `enabledPlugins.<pkg>@<mk>` in `~/.claude/settings.json` (the shape smooai's `.claude/settings.json` uses). Otherwise core + `harness/claude-code/` are composed into the local `th-pkg` marketplace at `~/.smooth/pkg/claude-marketplace/plugins/<pkg>/` (a `directory` source) — overlay files replace core, except `hooks/hooks.json`, which is key-merged (M1). Skills, commands, agents, hooks and MCP then load natively. `rules/*.md` → `~/.claude/rules/<pkg>/`. If `<pkg>@<anything>` is already enabled, nothing is registered twice. |
| **codex**       | `skills/*` → `~/.codex/skills/` (symlink; copy on non-unix). `.mcp.json` + `plugin.json.mcpServers` → `[mcp_servers.<name>]` in `~/.codex/config.toml`. `harness/codex/config.toml` deep-merged into `~/.codex/config.toml` (comments and layout survive; every leaf key is recorded as owned). **M1:** `harness/codex/hooks.json` key-merged into `~/.codex/hooks.json` (Codex ≥ 0.153 reads the Claude hooks schema there); `rules/*.md` → a managed section in `~/.codex/AGENTS.md`.                                                                                                                                                                                                             |
| **opencode**    | `skills/*` → `~/.opencode/skills/`. MCP → `mcp.<name>` in `~/.config/opencode/opencode.json`. `harness/opencode/plugin.js` → `~/.config/opencode/plugins/<pkg>.js` (symlink). **M1:** `rules/*.md` → a managed section in `~/.config/opencode/AGENTS.md`.                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| **cursor**      | **M1.** No plugin system: `rules/<stem>.md` → `~/.cursor/rules/<pkg>/<stem>.mdc` with Cursor frontmatter (`description`, `globs` from `paths:`, `alwaysApply` when there are no paths); `harness/cursor/rules/<stem>.mdc` replaces that rendering, extra `.mdc` files there are copied as they are. MCP → `mcpServers.<name>` in `~/.cursor/mcp.json` (the Claude shape). Skills, commands and agents are not rendered.                                                                                                                                                                                                                                                                             |
| **(all)**       | `skills/*` → `~/.smooth/skills/` so `th` itself (`th skills`, the daemon) discovers them.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |

A harness whose marker dir (`~/.claude`, `~/.codex`, `~/.config/opencode`,
`~/.cursor`) is missing is skipped with a note — nothing is conjured into
existence.

Not rendered yet (see roadmap): commands/agents for Codex and OpenCode,
`--project` scope.

## M1 — overlays that merge instead of replace

### `hooks.json` key-merge

A `hooks.json` is `{"hooks": {Event: [{matcher?, hooks: [{type, command, …}]}]}}`.
Wherever `th pkg` combines two of them — core + `harness/claude-code/hooks/hooks.json`
for the composed Claude plugin, or `harness/codex/hooks.json` into the user's
`~/.codex/hooks.json` — it merges at three levels and drops nothing:

1. **Event** — events union; an event only one side has is taken as is.
2. **Matcher group** — groups are matched by their `matcher` string (absent
   and `""` are the same group, as Claude and Codex treat them); a group with
   a new matcher is appended.
3. **Hook** — hooks are appended to the matched group unless an identical
   command (or, for non-command hooks, an identical value) is already there.

The user's file keeps its order; ours land after theirs. Only the hooks that
were actually added are recorded as owned (`[[packages.<pkg>.hooks]]` with
`file`, `event`, `matcher`, `command`), so a `th prime` the user already had
on `SessionStart` is never ours to remove even when the overlay ships the
same line. `rm` removes exactly those entries, prunes groups and events left
empty, and keeps `"hooks": {}` so the file stays valid. Codex asks the user to
trust changed hooks once ("Hooks need review"); that dialog is expected after
the first `install`.

`smooth-agent` uses this for **SmoothFlow state from Codex sessions** (pearl
th-4ad334): its `harness/codex/hooks.json` runs `flow-hook.sh <Event> codex`
on `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`,
`PreCompact` and `SessionEnd`, and the daemon binds the id-less Codex row by
cwd on the first hook. `PermissionRequest` is deliberately not wired for
Codex: its decision wire format is not Claude's.

### Cursor rules

`rules/<stem>.md` (optional `paths:` list and `description:` in the
frontmatter) renders as

```
---
description: <description, or "<pkg>: <stem>">
globs: <paths, comma-joined>        # only when the rule has paths
alwaysApply: <true when it has none>
---
<body>
```

at `~/.cursor/rules/<pkg>/<stem>.mdc`, recorded with its sha256 like any
copied file (`status` reports an edited rule; `rm` leaves it in place with a
warning). Cursor is a first-class `--harness` value and gets the `th mcp
serve` entry from `th harness enable cursor`.

### `AGENTS.md` managed section

Codex (`~/.codex/AGENTS.md`) and OpenCode (`~/.config/opencode/AGENTS.md`)
read global instructions from a Markdown file with no include mechanism, so
`rules/*.md` are rendered into one marker-delimited block per package:

```markdown
<!-- th-pkg:smooth-agent -->
<!-- managed by `th pkg install smooth-agent` — edits inside these markers are overwritten on reinstall; `th pkg rm smooth-agent` removes the block -->

### Rust style

_Applies to: `**/*.rs`_

be terse

<!-- /th-pkg:smooth-agent -->
```

The rewrite is idempotent and **never touches text outside the markers**: an
existing block is replaced in place (wherever the user moved it), a new one is
appended after a blank line, and other packages' blocks are left alone. The
index records the file, the marker name, the block's sha256 and whether the
file was created by us (`created = true` → `rm` deletes the file again once the
block is gone and nothing else is left). `status` reports a block that is
missing or edited; `rm` leaves an edited block in place with a warning
(reinstall overwrites it — the markers say so).

### `th harness enable codex`, after M1

Beyond the MCP entry and the package rendering, `enable codex` adds `~/.smooth`
to `[sandbox_workspace_write].writable_roots` in `~/.codex/config.toml` (once,
with a comment): Codex's workspace-write sandbox otherwise fails every
`th pearls` / `th agent` write with "unable to open database file" (sqlite
error 14). Either spelling (`~/.smooth` or the absolute path) counts as
present. `th harness status` reports the hook wiring and the sandbox root;
`th harness disable codex` removes the hooks, skills, config keys and
`AGENTS.md` section `th pkg` recorded for Codex and leaves the sandbox root
(harmless without `th`).

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

[[packages.smooth-agent.hooks]]   # M1: one entry we merged into a hooks.json
harness = "codex"
file = "/Users/me/.codex/hooks.json"
event = "SessionStart"
matcher = ""
command = "/Users/me/.smooth/pkg/cache/smooth-agent@local/hooks/flow-hook.sh SessionStart codex"

[[packages.smooth-agent.sections]]   # M1: a managed AGENTS.md block
harness = "codex"
file = "/Users/me/.codex/AGENTS.md"
name = "smooth-agent"
sha256 = "…"
created = false                   # true → rm deletes the file once the block is gone and it is empty
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
  hash mismatches, missing keys, missing hook entries, missing or edited
  managed sections — plus which customization points (`claude-code/hooks`,
  `codex/config.toml`, `codex/hooks.json`, `opencode/plugin.js`,
  `cursor/rules`, `rules`) the package provides.
- **Hooks are never translated.** Claude blocks only on exit 2, Copilot uses
  JSON, Cursor has none — so hooks stay per-harness customization points,
  shipped as explicit shims (`harness/opencode/plugin.js` is one,
  `harness/codex/hooks.json` another: Codex happens to read the Claude schema,
  but the overlay is still written for Codex on purpose).

## `th harness enable` is now sugar

`th harness enable codex|opencode|cursor` runs `th pkg install <smooth-agent
checkout> --harness <x>` plus the MCP entry and the harness's extras. The
checkout is, in order: the path a previous `th pkg install <path>` of
smooth-agent used (a repo checkout — re-run `enable` to re-render from it),
the newest Claude plugin cache, the marketplace clone. `claude-code` keeps its
`claude plugin install` path and the statusline check. `th harness disable
<h>` removes the MCP entry plus everything `th pkg` recorded for that harness
(`pkg::rm_harness`); `th pkg rm smooth-agent` removes the package from every
harness at once.

## Roadmap (pearl th-55b2c7)

- **M1 — overlays (shipped, pearl th-6ad314):** `hooks.json` key-merge instead
  of replace, rules for Cursor (`.mdc` frontmatter, overlay overrides), the
  `AGENTS.md` managed section, `harness/codex/hooks.json` → `~/.codex/hooks.json`
  (th-4ad334). Still owed from the M1 list: settings fragments for Claude,
  `--project` scope.
- **M2 — commands/agents** for Codex (`~/.codex/prompts`) and OpenCode
  (`command/`, `agent/`), `th pkg pack`, `th pkg update`, richer drift repair.
- **M3 — hosted marketplace:** `smoo pkg publish` serving a per-org
  `marketplace.json`; `ext_trust` content-hash pinning.

Stolen deliberately from the references (`~/dev/refs/apm`, `~/dev/refs/OpenPackage`):
deployed-file hashes, tracked-key deep merge. Skipped deliberately: DSLs, SBOM,
40-platform tables, registries, semver ranges, lockfiles beyond `index.toml`.
