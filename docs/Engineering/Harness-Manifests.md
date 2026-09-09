# Harness manifests — any coding agent CLI in SmoothFlow

Pearl th-0f6126 (epic th-faa590). A **harness manifest** is one TOML file
that tells the SmoothFlow engine everything it needs to run a coding agent
CLI as a supervised session: where the binary is, how to launch and resume it,
how its state is learned, how to steer and kill it, and where `th pkg` renders
skills/rules/MCP for it. The engine's launch table, binary resolver and pane
scraper read manifests — nothing about `claude`, `codex` or `opencode` is
hard-coded any more. Four ship built in:

| name       | display     | state    | prompt                   | session id                                           | resume               |
| ---------- | ----------- | -------- | ------------------------ | ---------------------------------------------------- | -------------------- |
| `claude`   | Claude Code | `hooks`  | positional               | pre-assigned (`--session-id <uuid>`)                 | `--resume <id>`      |
| `opencode` | OpenCode    | `hooks`  | `--prompt`               | learned from the plugin's first hook (by cwd)        | `--session <id>`     |
| `codex`    | Codex       | `hooks`  | positional               | learned from a hook (by cwd) once `hooks.json` posts | `resume <id>`        |
| `th-code`  | th code     | `native` | pasted into the composer | pre-assigned (`SMOOTH_FLOW_SESSION` in the pane env) | relaunch (see below) |

Source: `crates/smooth-flow/harnesses/*.toml` (embedded with `include_str!`),
loader + types in `crates/smooth-flow/src/harness.rs`.

## Where manifests live — precedence

Lowest to highest; a later file with the same `name` **replaces** the earlier
one wholesale (no field merging — copy the built-in and edit it).

1. built-in (`crates/smooth-flow/harnesses/`)
2. `~/.smooth/harnesses/<name>.toml` — `th harness add` writes here
3. `<project>/.smooth/harnesses/<name>.toml` — the main checkout's
4. `th pkg` packages: `<package root>/harness/<name>/harness.toml` for every
   package in `~/.smooth/pkg/index.toml`

The engine re-reads the set on every list/launch (they are a handful of small
files), so `th harness add` needs no daemon restart. A file that fails to
parse is skipped with a warning in the daemon log and reported by
`th harness list`; it never breaks the others.

## Schema

```toml
name = "aider"                # required — lowercase letters, digits, dashes; the session `kind`
display_name = "Aider"        # default: name

[binary]
names = ["aider"]             # required — PATH candidates, first hit wins; names[0] is the bare fallback
prefer_paths = [".local/bin/aider"]           # $HOME-relative, tried BEFORE PATH (the real installs)
skip_path_patterns = ["cmux-cli-shims"]       # default — a PATH dir with this component is a shim, skipped

[launch]
argv = ["--model", "{model}", "--message", "{prompt}"]   # AFTER the resolved binary
prompt_as = "argv"            # argv | paste  (paste = bracketed-paste + Enter ~4 s after launch)
session_id = "learned"        # preassigned | learned

[launch.env]                  # optional, exported into the pane's shell before the exec
SOME_VAR = "{session_id}"

[resume]
argv = ["--resume", "{session_id}"]
mode = "resume_session"       # resume_session | relaunch_command (default)

[state]
source = "hooks"              # hooks | scrape | native

[state.hooks]
install = "how the hooks get wired — free text shown by `th harness show`"
[state.hooks.event_map]       # harness event → working | idle | needs_you | ended | ignore
# empty ⇒ the Claude Code table (UserPromptSubmit/PreToolUse/PostToolUse → working, Stop → idle,
# PermissionRequest / Notification(permission|question) → needs_you, SessionEnd → ended)

[state.scrape]                # case-insensitive regexes over the visible pane
working = ["esc to interrupt"]
idle = ["> "]
needs_you = ["\\(y/n\\)"]
usage_limit = ["quota exhausted, back at (?P<reset>\\d{1,2}(?::\\d{2})?\\s*[ap]m)"]
error = ["api error"]

[steer]
method = "bracketed_paste"    # bracketed_paste | stdin
submit_key = "Enter"

[kill]
signal = "TERM"
grace_ms = 3000

[install]                     # where `th pkg` renders for this harness (consumed by th pkg later)
skills_dir = "~/.aider/skills"
rules_dir = "~/.aider/rules"
mcp_config = "~/.aider/mcp.json"
```

### Placeholders

`{prompt}`, `{session_id}`, `{cwd}`, `{model}`, `{daemon_url}` — allowed in
`launch.argv`, `launch.env` values and `resume.argv`. Rendering rule: an argv
element whose placeholder is empty is **dropped, together with a bare `-flag`
literal immediately before it**. So `["--model", "{model}", "{prompt}"]`
renders to `["p"]` with no model, `["--model", "m", "p"]` with one, and `[]`
with neither. An env entry whose placeholder is empty is dropped. Anything
else is a validation error naming the field.

### Validation (all errors name the field)

- `name` shape; `binary.names` non-empty
- unknown fields and unknown enum values are errors (`deny_unknown_fields`)
- `prompt_as = "argv"` needs `{prompt}` in `launch.argv`; `"paste"` must not have it
- `resume.mode = "resume_session"` needs `resume.argv` with `{session_id}`
- `state.source = "scrape"` needs `working` and/or `idle` patterns
- every scrape pattern must compile (each gets `(?i)`)
- `steer.submit_key`, `kill.signal` non-empty

### Scrape precedence (same as the shared detector)

A `working` hit in the **last 12 non-blank lines** wins; then `usage_limit`
anywhere (its `reset` capture, if any, is parsed for the resume time —
otherwise the whole pane is); then `needs_you`; then `error`; then `idle` in
the tail; else unknown. For `hooks`/`native` sources the engine scrapes only
limits and approvals; working/idle come from the harness.

### State sources

- **hooks** — the harness posts to `POST /api/flow/hooks` (`{harness, event,
session_id, cwd, payload}`). `install` says how that gets wired
  (`th harness enable <x>`). `Session.state_source` reads `hooks` once the
  first event lands, `inferred` (scraping) before.
- **scrape** — pane scraping only; `state_source` stays `inferred`.
- **native** — the harness is ours and reports its own turns. What it means
  **today** for `th code`: the engine exports `SMOOTH_FLOW_SESSION=<id>` and
  `SMOOTH_URL=<daemon>` into the pane; `th code` (crates/smooth-code
  `FlowReporter`) POSTs `turn_start` before each `TaskStart` and `turn_end` on
  `TaskComplete`/`TaskError`, which the manifest's `event_map` turns into
  working/idle. No plugin, no hook install, no working/idle scraping.
  Permission asks are **not** reported: `th code` runs the daemon in its
  auto-mode posture and renders no approval UI, so there is nothing to map.
  The daemon has no in-process turn-end seam (the operator's `LocalServer`
  owns the WS `send_message` path); when the engine exposes one, the reporter
  can move daemon-side without touching the manifest.

### `th code` limitations

- **Prompt is pasted** (`prompt_as = "paste"`, ~4 s after launch) because
  `th code` has no prompt flag in TUI mode.
- **Resume relaunches** (`mode = "relaunch_command"`): `th code --resume`
  takes a saved-session query, not the daemon conversation id the engine
  could learn. The relaunched TUI starts a fresh conversation.
- Needs the daemon's local token: the pane inherits `HOME`, so
  `~/.smooth/operator-token` is found the way any `th code` finds it.

## Sort / hide preferences

`{order: [names], hidden: [names]}` in flow.db (`config` table, key
`harness_prefs`). `order` puts those names first, in that order; the rest
follow in registry order. Hidden harnesses keep their manifest but leave every
picker.

- `GET /api/flow/harnesses` → `{harnesses: [{name, display_name, kind,
installed, binary_path, state_source, hidden, order_index, reason, origin}]}`
  (all, hidden flagged)
- `PUT /api/flow/harnesses/prefs {order?, hidden?}` → the same list; each
  half replaces only when present; unknown names are refused. Broadcasts
  `flow.harnesses`.
- `flow.hello.harnesses` — the visible list in order (what pickers render);
  `flow.harnesses {harnesses}` on change.

## `th harness`

```bash
th harness list [--all] [--json]     # in picker order; --all shows hidden; --json for scripts
th harness show <name> [--json]      # origin, resolved binary, the TOML
th harness hide <name> / unhide <name>
th harness order th-code claude      # these first, in this order
th harness add <file.toml | dir | owner/repo[/subdir][#ref]> [--force]   # validates, copies to ~/.smooth/harnesses
th harness enable|status|disable <provider>   # unchanged — this machine's toolbox setup
```

`list` reads the daemon (prefs applied) when it runs, else the files with a
note; `hide`/`unhide`/`order` need the daemon.

## Adding one by hand

1. `th harness show claude > ~/.smooth/harnesses/mytool.toml` and edit —
   or write from the schema above. Set `name`, `binary.names`, `launch.argv`.
2. Start with `state.source = "scrape"` and two patterns captured from a
   real pane (`th flow snapshot <id>` after `th flow new --kind mytool`);
   promote to `hooks` once the tool can post to `/api/flow/hooks`.
3. `th harness list` — it appears with the resolved binary (or the reason
   it didn't resolve). `th flow new --kind mytool --prompt "say ok"`.
4. To ship it: put it at `harness/<name>/harness.toml` in a `th pkg` package.

The agentic version of this (draft from `--help`, validate against the
fake-agent rig, install) is phase 2 of th-faa590.
