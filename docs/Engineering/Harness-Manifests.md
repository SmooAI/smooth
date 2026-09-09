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

## Agentic add — `th harness add --agentic <name>` (pearl th-473294)

Big Smooth can write the manifest itself. The daemon tool `add_harness(name,
binary_hint?, docs_url?, max_iterations?, force?, install_unverified?,
model?)` is the agentic loop; every piece it composes is deterministic and
unit-tested in `smooth-flow`:

1. **Probe** — resolve the binary (`~/.local/bin`, `~/.cargo/bin`, … before
   `PATH`, skipping cmux shims), run `--help` (`-h` / `help` fallbacks) and
   `--version`; fetch `docs_url` as markdown through `th crawl scrape` (the
   `crawl` tool's egress rules apply).
2. **Facts** — `harness_draft::parse_help` reads clap / commander / yargs /
   argparse layouts into flags, subcommands and positionals, then
   `argv_candidates` ranks launch shapes: an interactive positional prompt
   (80), an "…and stay interactive" flag (70), a plain `--prompt`/`--message`
   flag that is not a batch mode (60), else **paste** (40). Print/headless/
   "then exit" flags are never the prompt slot. `--session-id` ⇒
   `preassigned`; `--resume <id>` / `resume <id>` ⇒ `resume_session`; a
   `hooks` subcommand ⇒ "documents hooks". The best candidate becomes the
   skeleton TOML.
3. **Draft** — the daemon's model (`operator::agent_llm_config`, the coding
   route) gets the schema, the built-in `claude.toml` as the reference, the
   help text, the docs, the ranked candidates and the skeleton, and answers
   with one ```toml block. Parse errors are fed straight back.
4. **Validate** — `harness_validate::validate` runs the draft on a **private
   engine** (`EngineDriver::private`: its own `flow.db`, its own
   `tmux -L smooth-flow-validate-<pid>-…` server, a scratch `$HOME` holding
   only the draft, a scratch git repo as the worktree — the user's flow.db,
   tmux server and `~/.smooth/harnesses/` are never touched). The state
   machine proves, in order: launch · working observed · first turn idle ·
   steer acknowledged · steered turn idle · kill+resume came back · the
   relaunch argv carried the session id. Each step ends **proven**,
   **unproven** (could not be shown either way — e.g. no session id was
   learned because state is scraped) or **failed** (shown not to work, with
   the pane tail). Budgets: 60 s to boot, 120 s per turn.
5. **Iterate** — on failure the verdict, the last 12 pane lines and an idle
   pattern derived from that pane (`scrape_from_panes`) go back to the
   drafter, up to `max_iterations` (default 3, max 6). The attempt with the
   most proofs wins.
6. **Install** — a _usable_ draft (launch + idle proven) is written to
   `~/.smooth/harnesses/<name>.toml` (`force` to replace) with a provenance
   header; `install_unverified` writes the best draft regardless. The report
   lists the manifest, every proven step, every unproven/failed step with
   why, and next steps (`th flow new --kind <name>`, wire hooks, …).

`th harness add --agentic <name> [--binary <exe>] [--docs <url>]
[--iterations N] [--force] [--install-unverified] [--model <m>]` drives this
over the daemon's canonical WebSocket (one `send_message` turn asking the
agent to call `add_harness` with exactly those arguments): tool progress on
stderr, the agent's report on stdout, then the installed manifest line.

**Provider gate.** Drafting needs a model. `GET /api/llm/provider` reports
`{configured, source, model, gateway_host, restart_required, options}` (never
a key). With nothing configured the tool answers a structured
`needs_provider` report and the CLI prompts:

- **Smoo AI Gateway (recommended)** — `smoo auth login` if needed, mint the
  org's `llm.smoo.ai` key (`/llm-gateway/create-key`; on 409 offer a rotate
  or paste the existing key), save it as the `smooai-gateway` provider in
  `~/.smooth/providers.json` (other providers survive; coding default
  `deepseek-v4-flash`).
- **Bring your own key** — `th model login`.

The daemon reads its gateway **once at boot**, so after a provider is saved
the gate answers `configured` + `restart_required` and the CLI says so:
`th down && th up`, then rerun. Non-TTY runs get the two commands instead of
a prompt.

What it cannot prove is reported, not assumed: a CLI that opens an auth /
trust dialog on first run stops at `first turn reached idle — FAILED: the
harness is waiting on an approval/auth/trust prompt…` with the pane, and is
not installed unless `--install-unverified`. Sign the CLI in (or trust the
directory) and rerun.

Proven on this machine (2026-09-09): `gemini` (`@google/gemini-cli` via npm),
`aider` (`aider-chat` via `uv tool`, Python 3.12) and `cursor-agent` — see the
PR for the manifests each run produced.
