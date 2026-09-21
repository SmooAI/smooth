# Harness manifests — any coding agent CLI in SmoothFlow

Pearl th-0f6126 (epic th-faa590). A **harness manifest** is one TOML file
that tells the SmoothFlow engine everything it needs to run a coding agent
CLI as a supervised session: where the binary is, how to launch and resume it,
how its state is learned, how to steer and kill it, and where `th pkg` renders
skills/rules/MCP for it. The engine's launch table, binary resolver and pane
scraper read manifests — nothing about `claude`, `codex` or `opencode` is
hard-coded any more. Eight ship built in:

| name       | display     | state    | prompt                     | session id                                           | resume                                       |
| ---------- | ----------- | -------- | -------------------------- | ---------------------------------------------------- | -------------------------------------------- |
| `claude`   | Claude Code | `hooks`  | positional                 | pre-assigned (`--session-id <uuid>`)                 | `--resume <id>`                              |
| `opencode` | OpenCode    | `hooks`  | `--prompt`                 | learned from the plugin's first hook (by cwd)        | `--session <id>`                             |
| `codex`    | Codex       | `hooks`  | positional                 | learned from a hook (by cwd) once `hooks.json` posts | `resume <id>`                                |
| `th-code`  | th code     | `native` | pasted into the composer   | pre-assigned (`SMOOTH_FLOW_SESSION` in the pane env) | relaunch (see below)                         |
| `aider`    | Aider       | `scrape` | pasted once idle           | —                                                    | `--restore-chat-history` (`continue_latest`) |
| `goose`    | goose       | `scrape` | `run --interactive --text` | pre-assigned (`--name <id>`)                         | `session --resume --name <id>`               |
| `crush`    | Crush       | `scrape` | pasted once idle           | —                                                    | `--continue` (`continue_latest`)             |
| `cline`    | Cline       | `scrape` | positional (`--tui`)       | —                                                    | relaunch                                     |

The last four have no hooks SmoothFlow can receive (aider, goose) or hooks it
does not wire yet (crush, cline); their state comes entirely from ordered
`[[state.scrape.rules]]` — see [Scrape-only harnesses](#scrape-only-harnesses-th-e77603).

Seven more CLIs that report real lifecycle events ship built in too: `gemini`,
`qwen`, `cursor-agent`, `copilot`, `droid`, `amp`, `pi`. See
[Hook-capable harnesses](#hook-capable-harnesses-th-b00115) for why each is
shaped the way it is.

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
mode = "resume_session"       # resume_session | continue_latest | relaunch_command (default)

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
tail_lines = 12               # the live window for the flat working/idle lists and where="tail" rules

[[state.scrape.rules]]        # ordered; the FIRST rule that fires decides, BEFORE the flat lists
name = "aider-question"       # shown in the needs-you detail ("approval prompt on screen (aider-question)")
state = "needs_you"           # working | idle | needs_you (alias: permission) | usage_limit | error
match = ["\\(y\\)es/\\(n\\)o.*:\\s*$"]  # any-of, (?im): ^ and $ anchor each line
where = "last_line"           # tail (default) | pane | last_line | cursor_line | title
# lines = 8                   # window for where = "tail"
# all = ["model:"]            # every one must also hit the window
# unless = ["esc cancel"]     # void if any hits the window
# unless_below = ["^>"]       # void if one hits a line BELOW the last `match` line
# quiet_ms = 1500             # the pane text has not changed for ≥ this long
# changed_within_ms = 5000    # the pane text changed less than this long ago
# alternate_screen = true     # require the alternate screen on / off
# spinner = true              # require / forbid a braille or ◐◑◒◓ frame in the window

[steer]
method = "bracketed_paste"    # bracketed_paste | stdin
submit_key = "Enter"
submit_delay_ms = 0           # pause between the paste and submit_key (≤ 5000); gemini needs 300
# How flow.approve answers a SCRAPED approval prompt (no pending hook
# request), as tmux key names pressed in order, 250 ms apart (th-5a2314).
# Defaults are Claude Code's numbered menu: ["1"] / ["2"] / ["Escape"].
approve_keys = ["y", "Enter"]        # allow this once
allow_session_keys = ["y", "Enter"]  # allow and stop asking — repeat approve_keys if there is no such option
deny_keys = ["n", "Enter"]

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
- `state.source = "scrape"` needs `working` and/or `idle` patterns, or a
  `working`/`idle` rule
- `resume.mode = "continue_latest"` needs `resume.argv` without
  `{session_id}` or `{prompt}`
- every rule needs `match`, `all` or a signal; `lines` only with
  `where = "tail"` (1–500); no `unless_below` on a title; `quiet_ms` and
  `changed_within_ms` together must be satisfiable
- every scrape pattern must compile (flat lists get `(?i)`, rules `(?im)`)
- `steer.submit_key`, `kill.signal` non-empty; `steer.submit_delay_ms` ≤ 5000;
  `steer.approve_keys`, `allow_session_keys`, `deny_keys` each at least one
  non-empty key

### Scrape precedence (same as the shared detector)

A `working` hit in the **last 12 non-blank lines** wins; then `usage_limit`
anywhere (its `reset` capture, if any, is parsed for the resume time —
otherwise the whole pane is); then `needs_you`; then `error`; then `idle` in
the tail; else unknown. For `hooks`/`native` sources the engine scrapes only
limits and approvals; working/idle come from the harness.

One refinement over "an approval anywhere" (th-473294): a `needs_you` hit
with an `idle` hit on a **later line** is not pending. Scrolling CLIs (aider)
keep the answered question on screen — `… (Y)es/(N)o [Yes]: y` — and print
their `>` prompt under it; a modal dialog (gemini's folder trust, Claude
Code's approval box) has nothing idle below it, so it still wins.

### Scrape rules (th-e77603)

`[[state.scrape.rules]]` run first, in file order; the first rule whose
**every** condition holds decides, and its `name` rides along into the
needs-you detail. Only when no rule fires do the flat lists above run, with
their precedence unchanged — a manifest without rules reads exactly as before.

What a rule sees, per supervision tick (`crate::scrape::PaneObservation`):

- the visible pane, `capture-pane -p` (escape sequences and `\r` stripped
  first, so a `-e` capture or a raw PTY tail reads the same);
- `#{pane_title}` (OSC 0/2), `#{alternate_on}`, `#{cursor_y}` — one
  `display-message` per tick; a rule needing one that tmux could not report
  simply does not fire;
- how long the pane text has held still: the engine hashes each capture and
  remembers when it last changed. **The first look is unknown, and a time
  condition never holds on unknown time.**

Scopes: `tail` = the last `lines` non-blank lines; `pane` = everything;
`last_line` = the last non-blank line; `cursor_line` = the row the cursor is on
(the last non-blank line when unknown); `title` = the pane title (tmux reports
the host name when a program never set one — don't match on that).

**Timing under load.** The daemon ticks every 2 s, and this machine runs many
agents at once, so time conditions are coarse by construction: `quiet_for` is
measured from the tick that first SAW the change, so it under-reports real
quiet by up to one tick, and a slow tick never manufactures a change (the text
is compared, not timestamps). Rules of thumb: `quiet_ms` ≥ 1000 only as a
guard on a prompt match, never alone; `changed_within_ms` ≥ 2 × the tick
(5000) and scoped to `cursor_line` with an `unless` for the composer, so a
user typing in the pane is not "working". A turn stalled on the network with
no marker reads **unknown** — the engine keeps the previous state rather than
guessing idle.

### State sources

- **hooks** — the harness posts to `POST /api/flow/hooks` (`{harness, event,
session_id, cwd, payload}`). `install` says how that gets wired
  (`th harness enable <x>`). `Session.state_source` reads `hooks` once the
  first event lands, `inferred` (scraping) before. Every post must carry
  `X-Smooth-Flow-Hook-Token`: the contents of the file the engine names in
  the pane's `SMOOTH_FLOW_HOOK_TOKEN_FILE` (th-91d032). Without it, hooks for
  a session SmoothFlow launched are refused. A new hook integration reads that
  file (hex only) and sends the header, as `flow-hook.sh` does. See
  SmoothFlow.md § Hook authentication.
- **scrape** — pane scraping only; `state_source` stays `inferred`.
- **native** — the harness is ours and reports its own turns. What it means
  **today** for `th code`: the engine exports `SMOOTH_FLOW_SESSION=<id>`,
  `SMOOTH_URL=<daemon>` and `SMOOTH_FLOW_HOOK_TOKEN_FILE` into the pane; `th code` (crates/smooth-code
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

## Adoption — a harness started outside SmoothFlow (th-c103c1)

Because the `th pkg` hook overlay posts `{harness, event, session_id, cwd}`
from every harness, a `claude` or `codex` started in a plain terminal can be
**adopted** into the fleet: the engine infers its worktree, project, branch,
pearl and Jira key from the `cwd` the hook already carries, and creates a row.

Two things a manifest author should know:

- The `harness` field a hook sends must resolve to a manifest name. The
  `-code` spelling is stripped (`claude-code` → `claude`); anything else must
  match the manifest name exactly, or the session is not adopted.
- An adopted row is NOT engine-owned: no pane to attach, no resume, no kill,
  and supervision skips it. Its `[resume]` and `[launch]` blocks are never
  used; only `[state]` matters.

Off by default — `th flow adopt on`. Full rules and guards:
[`SmoothFlow.md`](../Architecture/SmoothFlow.md#zero-friction--inference-and-adoption-th-c103c1).

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
th harness doctor [name] [--json] [-v]        # does each harness WORK here? read-only verdict + the fix (th-3cabf6)
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

**First-run prompts.** The private engine runs the CLI in a scratch folder,
so the validator meets exactly what a user meets on a fresh machine: gemini's
_"Do you trust the files in this folder?"_ then its auth-method dialog,
aider's _"Add .aider\* to .gitignore? (Y)es/(N)o"_. Before the first idle (and
after a resume — never during a steer, where a prompt is the harness's real
answer) it answers those the way a person would: `harness_validate::
FIRST_RUN_PROMPTS` names each dialog and the key that accepts its default
(`Enter`; the exceptions, _"open the documentation url?"_ and _"see what's
new in this version?"_, get `n` so no browser pops mid-validation). A visible
prompt is checked _before_ the scraped state is believed (gemini paints its
`>` composer a beat before the trust dialog covers it), a scraped `idle` must
hold for three polls, the prompt **nearest the cursor** is the pending one
(scrolling CLIs keep the answered questions visible above the live one, with
the typed answer echoed onto them), each is answered once while it stays on
screen, at most `MAX_ANSWERS` (4) per run. Every answer is recorded in the verdict (`answered`), shown in
the report (`◐ answered a first-run prompt by pressing its default: …`) and
handed to the drafter with the rule that `needs_you` must match each one — a
real SmoothFlow session must surface them to the user, never skip them with a
flag.

Two things are never pressed: a **sign-in** (_"Press any key to sign in…"_,
_"Sign in with Google"_ highlighted — `Enter` there starts a browser login on
the user's desk; gemini's auth dialog is answered only when it says an API
key was detected, because that is then the default), and any `needs_you` the
table does not name (an approval, an unknown dialog) — that is the harness
asking, and it stays the blocking reason with the pane tail.

What it cannot prove is reported, not assumed: a CLI that needs a sign-in, or
a dialog the default key does not clear, stops at `first turn reached idle —
FAILED: …` with the pane tail (and what was already pressed), and is not
installed unless `--install-unverified`. Sign the CLI in and rerun.

Smoke-tested on this machine (2026-09-09) against a private daemon
(`deepseek-v4-flash` via llm.smoo.ai), one `th harness add --agentic <cli>` each:

| CLI                                       | version    | result                  | reached                                                                                                                                                                        | blocker                                                                                                               |
| ----------------------------------------- | ---------- | ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------- |
| `gemini` (`@google/gemini-cli`, npm)      | 0.59.0     | **installed** (partial) | launch, working, first idle, **steered turn idle, kill+resume, resume reused the session id**; steer-acknowledged unproven (its turn is too fast to catch `working` mid-steer) | folder-trust dialog — answered by the validator                                                                       |
| `aider` (`aider-chat`, `uv tool`, py3.12) | 0.86.2     | drafted, not installed  | launch; three first-run prompts answered (`.gitignore` → Enter, docs → n, "what's new" → n)                                                                                    | `prompt_as="paste"` pastes on a timer and lands inside a first-run question, so the turn never runs — pearl th-d2a1e4 |
| `cursor-agent`                            | 2025.10.01 | drafted, not installed  | launch                                                                                                                                                                         | browser sign-in wall ("Press any key to sign in…") — never pressed by design; sign in once, then rerun                |

`gemini`'s installed manifest launches with `--session-id {session_id} --model
{model} {prompt}` (prompt as argv, preassigned session id), resumes with
`--resume {session_id}`, scrapes state (its `hooks` subcommand is undocumented
for our transport), steers by bracketed paste, kills with TERM.

## Scrape-only harnesses (th-e77603)

A coding CLI with no lifecycle hooks is known to SmoothFlow only through its
terminal. The four scraped built-ins are written in `[[state.scrape.rules]]`
against panes **captured live** from the real CLIs, and each was then driven
end to end through the engine.

### What shipped, and how it was proven

Captures and drives ran on 2026-09-14 with a fresh scratch `$HOME` per CLI,
isolated installs (`uv tool` / `npm --prefix` / release tarball into a scratch
dir, deleted afterwards) and a **local mock OpenAI-compatible server** — no
account, no API key, no real model. "Live" below means the built-in manifest
ran on a private engine (`tests/scrape_live.rs`) through: launch → first-run
questions read needs-you → working → idle; steer → working → idle; a tool or
edit request → needs-you → deny → idle; kill + resume → back.

| harness  | version           | proof        | first-run / walls met                                                                                               | notes                                                                                                                                   |
| -------- | ----------------- | ------------ | ------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `aider`  | aider-chat 0.86.2 | **live**     | `.gitignore` question, "Open documentation url?"                                                                    | prompt pasted only after the questions (th-d2a1e4); streaming has no marker, so working = the pane changing under a non-composer cursor |
| `goose`  | 1.50.0            | **live**     | anonymous-usage question (◆); tool approval needs `GOOSE_MODE=approve`                                              | the session is named with the engine id → a true `resume_session`                                                                       |
| `crush`  | 0.94.2            | **live**     | provider/model picker on a fresh install; "initialize this project?"                                                | the permission modal sits over an `esc cancel` footer — needs-you is ordered first                                                      |
| `cline`  | 3.0.61            | **live**     | "Connect a model provider" sign-in screen (configured through Bring-your-own-provider → the mock); ClinePass upsell | launched `--auto-approve false`; the composer looks idle all turn, the braille spinner above it is the signal                           |
| `auggie` | 0.36.0            | fixture only | "Login to continue / Press return to open your browser" — **not pressed**                                           | no built-in: nothing past the wall could be observed                                                                                    |
| `kiro`   | kiro-cli 2.21.4   | fixture only | "You are not logged in. Login now?" — **not pressed**                                                               | no built-in; Kiro has agent hooks (cmux wires `kiro-cli chat --agent cmux`), a better fit for a hooked manifest                         |

crush and cline also document hooks (`crush.json` hooks, `cline --hooks-dir`);
their manifests say so and can move to `source = "hooks"` when those are wired.
The rules stay useful for needs-you and limits either way.

### Fixtures and tests

- `crates/smooth-flow/tests/fixtures/scrape/<harness>/<case>.pane` — 44 real
  captures. The header carries what the engine observes alongside the text
  (title, alternate screen, cursor row, how long the pane had been quiet), the
  expected verdict and the rule that must decide it. The scratch path is
  scrubbed to same-width filler. Hard cases are marked `HARD:` — a spinner
  mid-frame, aider's `> prompt` echo that looks like a composer while the model
  streams, cline's idle-looking composer during a turn, crush's and cline's
  approval over a still-busy footer, `capture-pane -e` ANSI noise, resized panes.
- `tests/scrape_fixtures.rs` — every fixture reads as its header says; every
  scraped built-in has working, idle and needs-you fixtures; sign-in walls never
  read idle or working under **any** built-in; verdicts survive a taller pane,
  space-padded lines and SGR colour; idle holds as quiet grows; needs-you never
  depends on time.
- `src/scrape.rs` unit tests — scopes, `all` / `unless` / `unless_below`, time
  and terminal signals, spinner glyphs, ANSI stripping, and a generated-pane
  property test (window nesting `last_line ⊆ tail ⊆ pane`, `unless` and time
  conditions only remove hits, blank lines never change a verdict).
- `engine::tests::live_scraped_session_waits_to_paste_and_clears_needs_you` — a
  fake scrape-only CLI in real tmux: no paste into a first-run question, the
  question reads needs-you, answering it in the pane clears needs-you on idle,
  then the paste lands and the turn reads working → idle.
- `tests/scrape_live.rs` (`#[ignore]`) — the real-CLI drive above; its module
  docs list the environment it needs.

To add a harness the same way: capture the pane for each state (`th flow
snapshot`, or `tmux capture-pane -p` plus `display-message -p
'#{alternate_on}|#{cursor_y}|#{pane_title}'`), write the rules, and commit the
panes as fixtures.

### Paste waits for idle (th-d2a1e4)

`prompt_as = "paste"` used to paste 4 s after launch whatever the pane showed,
so aider's first-run question swallowed the prompt. A manifest that can scrape
an idle composer (an `idle` pattern or rule) now pastes on the first tick after
1 s that reads **idle**, never while the pane reads **needs-you**, and after 90 s
of neither pastes anyway. Manifests with no idle signal (`th-code`) keep the
fixed 4 s.

A scraped needs-you (not a hook's pending approval) also ends when the pane
next reads idle — the user answered in the terminal, not through
`flow.approve`.

### `resume.mode = "continue_latest"`

For CLIs whose session id the engine cannot learn from a pane but which can
continue "the most recent conversation here" (`crush --continue`,
`aider --restore-chat-history`). `resume.argv` is appended to the original argv
when the prompt was pasted (so `--model` survives), and to the binary alone
when the prompt was an argument (so it is not sent twice). Every SmoothFlow
session has its own worktree, so "latest here" is that session's.

### Known limits

- A scrolling CLI that is booting reads **working** (its pane is changing and
  the cursor is not on a composer) until its composer settles.
- A turn stalled with no marker and no new text reads **unknown**; the engine
  keeps the previous state.
- `flow.approve` on a scraped prompt presses the manifest's `[steer]`
  keys (th-5a2314). A prompt shaped differently from the one those keys
  were written for (a new dialog in a new CLI version) can still need an
  answer in the pane. The needs-you clears on the next idle either way.

| harness | approve     | allow for session | deny                    | from the prompt                                   |
| ------- | ----------- | ----------------- | ----------------------- | ------------------------------------------------- |
| claude  | `1`         | `2`               | `Escape`                | the numbered menu (the default)                   |
| aider   | `y` `Enter` | `y` `Enter`       | `n` `Enter`             | `Create new file? (Y)es/(N)o [Yes]:`              |
| goose   | `Enter`     | `Down` `Enter`    | `Down` `Down` `Enter`   | `● Allow / ○ Always Allow / ○ Deny / ○ Cancel`    |
| crush   | `Enter`     | `Right` `Enter`   | `Right` `Right` `Enter` | `Allow · Allow for Session · Deny`, ←/→ to choose |
| cline   | `y`         | `y`               | `n`                     | `[y] Approve   [n] Deny`                          |

The deny column is what `tests/scrape_live.rs` pressed against the real CLIs.

### How cmux and orca detect state without hooks

Read from `~/dev/refs/orca` (102402e41e) and `~/dev/refs/cmux` (2bde0876f2).

**orca** — the OSC title is the primary hookless signal; the screen is the
fallback, bounded to what owns the bottom of it:

- `src/shared/agent-title-status.ts:182` `computeAgentStatusFromTitle` — a
  ladder over the terminal title: agent-specific glyphs (Gemini ✋ permission
  at `:201`, ✦/⏲ working, ◇ idle), any spinner frame ⇒ working (`:230`), then
  `action required` / `permission` / `waiting` ⇒ permission and boundary-aware
  `ready|idle|done` / `working|thinking|running` keywords
  (`agent-title-core.ts:34`). Spinner families: braille U+2800–28FF and quarter
  circles U+25D0–25D3 (`agent-title-core.ts:51`, `:56`), treated as activity,
  never identity (`isQuarterCircleSpinnerOnlyAgentTitle`).
- `src/renderer/src/lib/agent-status.ts:28` — title-scraped activity is gated on
  a live PTY: titles survive sleep, so a slept tab would read working forever.
- `src/main/runtime/terminal-wait-detection.ts` — the screen fallback.
  Blocked-prompt detection only within the last 12 non-blank lines
  (`LIVE_PROMPT_TAIL_LINES`, `:212`), 8 for cursor-agent's approval menu
  (`:172`), because answered dialogs stay in scrollback; a menu counts only if
  its choice lines END with a selectable key and the last choice is the last
  line (`:174`–`:207`). A live prompt BELOW a blocked signal dismisses it
  (`findDismissedStartupModalIndex`, `:73`) — the idea behind `unless_below`.
  cursor-agent emits no idle title, so its ready state is its `→` prompt with no
  braille spinner after the banner (`:101`).
- `src/main/runtime/runtime-terminal-idle-polls.ts:125` — the last resort for a
  hookless TUI: the foreground process is not a shell AND no output for
  `TUI_IDLE_QUIESCENCE_MS` = 3000 (`orca-runtime-postlude.ts:65`), polled every
  2000 ms (`:63`).
- Prompt delivery for hookless agents (`tui-agent-config.ts`: aider, goose,
  kiro-cli, crush, auggie and cline are all `stdin-after-start`) waits for
  DECSET 2004 (bracketed paste enabled) on the PTY, then 1500 ms of render quiet
  (`agent-draft-readiness.ts:9`, `agent-paste-draft.ts:70`), and presses Enter
  50 ms after the paste (`agent-paste-draft.ts:36`).

**cmux** — hooks first (`CLI/CMUXCLI+AgentHookCatalog.swift`, Kiro at `:119`);
beyond hooks it reads terminal-level signals, not screen text:

- OSC 9 / OSC 777 `notify` desktop notifications become attention
  (`Sources/RemoteTmuxNotificationOSCFilter.swift:30`, prefixes `:44`–`:45`).
- BEL: Ghostty's attention bell marks a background pane unread and flashes it;
  a bell in the focused pane is feedback and is ignored
  (`Sources/Workspace+AttentionFlashRouting.swift:133`).
- Title churn: a leading braille spinner frame is stripped before titles are
  compared, so animation is not a title change
  (`Packages/macOS/CmuxTerminalCore/Sources/CmuxTerminalCore/TitleChurn/TerminalTitleChurnFilter.swift:15`, frames `:41`).
- tmux `#{alternate_on}` + `#{pane_current_command}`: a non-shell foreground or
  the alternate screen means "a command is active"
  (`Sources/RemoteTmuxPaneForegroundState.swift:41`).

**What SmoothFlow takes, and what it can't yet.** Taken: bounded live windows,
dismissal by a later prompt, spinner glyph families, title and alternate-screen
signals, output quiescence, and orca's paste readiness (wait for the composer,
then paste). Not available through tmux 3.5: DECSET 2004 has no format
variable, and OSC 9/777 and BEL are consumed by tmux before `capture-pane`
sees anything; reaching them means reading the raw PTY stream (`pty.rs` already
holds one per attached client). None of the four shipped harnesses needed them.

### Claude Code's heuristics as rules

`smooth_tmux::detect::detect_state` (shared by `th claude` and the
claude/codex/opencode flat lists) is expressible in the rule language with
nothing added: five rules — working over a 12-line `tail`, then usage-limit,
needs-you and error over the `pane`, then idle over the `tail`.
`tests/scrape_fixtures.rs::claude_detect_rs_is_expressible_as_rules` checks
that rule set against `detect_state` on detect.rs's own panes, 5 000 generated
panes and every real capture. `detect.rs` itself is unchanged. The
th-473294 refinement the built-in manifests layer on top ("an approval above
an idle line is not pending") is `unless_below` in a rule.

## Supporting a harness — the conformance contract

Pearl th-3cabf6. "Supports harness X" means X passes this contract in CI, not
that someone launched it once. Every manifest in `harness::BUILTIN` (plus the
reference manifests in `crates/smooth-flow/tests/conformance/manifests/`) is
run by `crates/smooth-flow/tests/harness_conformance.rs` against
`smooth-flow-fake-agent` — a stand-in CLI that speaks **that manifest's**
mechanism, derived from the manifest itself — on a private engine: scratch
HOME / flow.db / worktree, its own tmux socket, an in-process
`POST /api/flow/hooks` listener. Never a real CLI, never the network, never
credentials.

```bash
cargo test -p smooai-smooth-flow --test harness_conformance -- --nocapture
SMOOTH_CONFORMANCE_ONLY=gemini,aider cargo test -p smooai-smooth-flow --test harness_conformance -- --nocapture
SMOOTH_CONFORMANCE_KEEP=1 …   # keep each scratch dir (fake.log, spec.json, flow.db) for a post-mortem
```

CI: the `Harness conformance` job in `pr-checks.yml` (Linux, tmux installed,
`SMOOTH_E2E_STRICT=1` so a missing tmux fails instead of skipping).

### The eight steps

| step         | what must hold                                                                                                                                                                                                                                                                                                                                 |
| ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `resolve`    | the fake, installed at `~/<prefer_paths[0]>` (or as `names[0]` on PATH when there are none), is what `binary` resolves to — with a decoy `names[0]` in a `cmux-cli-shims` dir **first** on PATH, which must never run                                                                                                                          |
| `launch`     | the engine's argv is `[bin] + render(launch.argv)`, and the fake can read its prompt (and a preassigned session id, from argv or `launch.env`) back out of it by matching the template: literals in order, a placeholder binds one token or is dropped together with a bare `-flag` before it, `--flag={x}` round-trips                        |
| `working`    | observed for the launch prompt (argv, or pasted ~4 s after launch)                                                                                                                                                                                                                                                                             |
| `idle`       | the turn ends, and the row's `state_source` is what `state.source` promises: `hooks`, `native`, or `inferred` for scrape                                                                                                                                                                                                                       |
| `steer`      | `flow.send` → `working` → `idle`                                                                                                                                                                                                                                                                                                               |
| `permission` | only when the manifest claims one (hooks with the Claude table, an `event_map` entry → `needs_you`, or `state.scrape.needs_you` patterns): the row reaches `needs_you` with a `request_id`, `flow.approve(allow)` reaches the harness — as the `PermissionRequest` long-poll's reply, or as the approval keystroke `1` — and the turn finishes |
| `resume`     | `kill(resume)` relaunches with `[bin] + render(resume.argv, session_id)` (the preassigned id, or the one **learned** from the first hook) or the original argv for `relaunch_command`, and the resumed process drives the **same row** through a steered turn back to `idle`                                                                   |
| `kill`       | `kill` leaves the row `done`, the process gone, the tmux session gone                                                                                                                                                                                                                                                                          |

### How the fake speaks your manifest

- **hooks, no `event_map`** — Claude Code's names: `SessionStart`,
  `UserPromptSubmit`, `PreToolUse`/`PostToolUse`, `Stop`, `PermissionRequest`
  (long-polled), with `harness = <name>`, the session id and cwd.
- **hooks / native with an `event_map`** — the map inverted: it posts the
  first event you map to `working`, `idle`, `needs_you` (payload
  `{"reason":"permission"}`) and `ended`. So the map **must** name at least
  one `working` and one `idle` event, and a `needs_you` ask must be
  answerable with the manifest's `[steer] approve_keys`.
- **permission (mapped and scrape)** — the fake reads the raw bytes of the
  manifest's `approve_keys` off the pane (`-icrnl`, so `Enter` is `\r`). The
  rig requires exactly those bytes, so an engine that ignored the manifest
  and pressed Claude's `1` fails `permission` for every non-Claude harness
  (th-5a2314). A key name the rig cannot map to bytes fails too, rather than
  passing on a guess.
- **scrape** — posts nothing; clears the screen and paints the fixture's
  screens verbatim.

### What a new harness provides

1. The manifest, in `crates/smooth-flow/harnesses/<name>.toml`, listed in
   `BUILTIN`. That alone enrolls it — no test code.
2. For `state.source = "scrape"` (optional otherwise): a fixture,
   `crates/smooth-flow/tests/conformance/<name>.toml`, holding screens
   **captured from the real CLI** (`th flow snapshot <id>`, ANSI stripped):

    ```toml
    [screens]
    boot = "…"       # optional
    working = "…"    # mid-turn — required when state.scrape.working is set
    idle = "…"       # at rest — required
    needs_you = "…"  # an approval prompt — required when state.scrape.needs_you is set
    ```

    Each screen is first checked against the manifest's own regexes (working →
    `Working`, idle → `Idle`, needs_you → `AwaitingApproval`; no screen may read
    as a usage limit) and then painted into a live pane the engine must follow.
    The fixture is how "our patterns match what the CLI really paints" stays
    true.

3. A row in `th harness doctor`'s knowledge table
   (`crates/smooth-cli/src/harness_doctor.rs` `known`) when the harness has an
   install command, a hook-install/trust state, or an auth file doctor can read
   without a prompt.

A failing run names the harness and the step with the pane tail, e.g.
``○ FAILED permission attention `permission` carries no request_id to approve``
— which is the engine bug this suite found on its first run (a permission ask
under a harness's own event name could not be approved; fixed in th-3cabf6).

### `th harness doctor` — the same question, on a real machine

The suite proves a manifest is right; doctor says whether it works **here**.
Read-only — it never installs, trusts a hook dialog or logs in:

| check     | degrades the harness when                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| --------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `binary`  | nothing resolves, or only a cmux CLI shim does (a shim in front of a real install is reported, not degrading)                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `app_env` | (macOS) it resolves from your shell but not under the SmoothFlow app's launchd PATH, or it is a `#!/usr/bin/env node` script whose interpreter the app cannot find                                                                                                                                                                                                                                                                                                                                                                                              |
| `version` | the binary cannot be executed (a failing `--version` only warns)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `hooks`   | known hooks harnesses: the smooth-agent plugin / flow hook is missing or stale, or a **project-scoped** smooth-agent pin in `~/.claude/plugins/installed_plugins.json` for an existing checkout is older than the user-scoped install (it shadows the user install there; fix: `cd <project> && claude plugin update smooth-agent@smooth --scope project`, which `th harness enable claude-code` now also runs for every such pin), OpenCode's plugin lacks the generic `event` hook, Codex's flow hooks are not **trusted** (`[hooks.state]` in `config.toml`) |
| `signal`  | never — a daemon that is not up only warns (`th up`)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `auth`    | Claude Code / Codex / th code are not signed in (env key, credentials file, or the login keychain item's presence). goose has no `GOOSE_PROVIDER` in `~/.config/goose/config.yaml`; crush has no `providers` in `~/.config/crush/crush.json` or `~/.local/share/crush/crush.json`; cline has no `~/.cline/data/settings/providers.json` (th-5a2314). aider without a provider key only warns, because the app's daemon may carry one this shell does not.                                                                                                       |

`--json` emits `{harnesses: [{name, verdict: works|degraded|not_installed,
reason, fix, binary, app_binary, cmux_shim, version, checks: [{id, level,
detail, fix}]}], app_path}` for the app.

## Hook-capable harnesses (th-b00115)

Seven CLIs report their own lifecycle through hooks, plugins or extensions,
so SmoothFlow reads their state instead of scraping the pane. Each has a
built-in manifest (`crates/smooth-flow/harnesses/<name>.toml`) and a `th pkg`
overlay in the smooth-agent package (`claude-plugins/smooth-agent/harness/`)
that maps its native events onto `POST /api/flow/hooks`. Nothing here
screen-scrapes working/idle once a hook has spoken. Scrape patterns stay as
the fallback before the first hook, and for usage limits and approvals.

### Reference matrix

How [orca](https://github.com/stablyai/orca) (`~/dev/refs/orca`, 2026-09-07)
and [cmux](https://github.com/manaflow-ai/cmux) (`~/dev/refs/cmux`,
2026-09-07) launch, resume and read state for each, and what the CLI's own
docs said on 2026-09-14. orca paths are under `src/`; cmux's hook catalog is
`CLI/CMUXCLI+AgentHookCatalog.swift` (`hooks`), its resume table is
`Packages/macOS/CMUXAgentLaunch/Sources/CMUXAgentLaunch/AgentResumeArgv.swift`
(`resume`).

| CLI (tested version)        | orca                                                                                                                                                                                                                                                                                                                                                                                                                      | cmux                                                                                                                                                                                          | CLI docs today                                                                                                                                                                                                                                                                                                                                     |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Gemini CLI** 0.59.0       | launch `gemini -i <prompt>` (`shared/tui-agent-config.ts:162`); resume `gemini --resume <id>` (`shared/agent-session-resume.ts:261`); state: command hooks `BeforeAgent`/`AfterAgent`/`BeforeTool`/`AfterTool` in `~/.gemini/settings.json` (`main/gemini/hook-service.ts:34`), no waiting state: "approvals are inline UI" (`:32`)                                                                                       | hooks `SessionStart`/`BeforeAgent`/`AfterAgent`/`SessionEnd` in `~/.gemini/settings.json` (`hooks:105-117`); resume `--resume <id>` (`resume:445`)                                            | `bundle/docs/hooks/reference.md`: stdin `session_id`, `cwd`; `Notification(notification_type: "ToolPermission")` is observability-only; exit 2 blocks, stdout must be JSON. `--session-id <uuid>` sets a new session's id; the positional query is interactive                                                                                     |
| **Qwen Code** 0.23.4        | launch bare `qwen`, prompt pasted (`tui-agent-config.ts:248`); no hooks, no resume                                                                                                                                                                                                                                                                                                                                        | not integrated                                                                                                                                                                                | `bundled/qc-helper/docs/features/hooks.md`: Claude Code's event names **and** decision protocol (`PermissionRequest` → `hookSpecificOutput.decision.behavior`) in `~/.qwen/settings.json`; `Notification.notification_type` `permission_prompt`/`idle_prompt`/`auth_success`. `--session-id`, `-r <id>`; positional = one-shot, `-i` = interactive |
| **Cursor Agent** 2025.10.01 | launch `cursor-agent <prompt>`, pre-writes workspace trust (`tui-agent-config.ts:222`); no resume id (`agent-session-resume.ts:241-245`); state: `beforeSubmitPrompt`/`stop`/`preToolUse`/`postToolUse`/`beforeShellExecution`/`beforeMCPExecution`/`afterAgentResponse` in `~/.cursor/hooks.json`, answering gates with `{"permission":"allow"}` (`main/cursor/hook-events.ts:3-25`)                                     | hooks `beforeSubmitPrompt`/`stop`/`afterAgentResponse`/`beforeShellExecution`/`afterShellExecution`/`postToolUseFailure` (`hooks:87-104`); resume `cursor-agent --resume <id>` (`resume:443`) | cursor.com/docs/agent/hooks: `{"version":1,"hooks":{event:[{command}]}}`; stdin `conversation_id`, `workspace_roots` (no `session_id`/`cwd`); gate hooks **fail closed** on empty/invalid stdout                                                                                                                                                   |
| **Copilot CLI** 1.0.83      | launch `copilot -i <prompt>`, pre-writes trust (`tui-agent-config.ts:268`); resume `copilot --resume=<id>` (`agent-session-resume.ts:290`); state: PascalCase hooks in `~/.copilot/hooks/orca.json`, `bash` + `timeoutSec` entries (`main/copilot/copilot-managed-hook-definitions.ts:10-40`, `hook-service.ts:29-36`); `PermissionRequest` "stays working" (`shared/agent-hook-listener/providers/copilot-events.ts:15`) | hooks `SessionStart`/`Stop`/`Notification`/`SessionEnd` in `~/.copilot/config.json` (`hooks:186-198`); resume `copilot --resume <id>` (`resume:465`)                                          | docs.github.com hooks-configuration: `~/.copilot/hooks/*.json` (`COPILOT_HOME`), `{"version":1}`; PascalCase events get `session_id`, camelCase get `sessionId`; `preToolUse` is fail-closed on non-zero exit. `--session-id <id>` sets a new session's UUID                                                                                       |
| **Factory Droid** 0.218.2   | launch `droid <prompt>` (`tui-agent-config.ts:228`); resume `droid --resume <id>` (`agent-session-resume.ts:277`); state: Claude-named hooks incl. `PermissionRequest` in `~/.factory/settings.json` (`main/droid/hook-service.ts:36-58`); `SessionStart` fires idle, no `Stop` on interrupt (`providers/droid-events.ts:23-53`)                                                                                          | hooks `SessionStart`/`Stop`/`Notification`/`SessionEnd` in `~/.factory/settings.json` (`hooks:212-224`); resume `droid --resume <id>` (`resume:469`)                                          | docs.factory.ai hooks reference: `~/.factory/hooks.json` or `settings.json` `hooks`; stdin `session_id`, `cwd`; exit 2 blocks. `droid [prompt]`, `-r/--resume [id]`; a session id can only be set in `droid exec`                                                                                                                                  |
| **Amp** 0.0.1789416054      | launch bare `amp`, prompt pasted (`tui-agent-config.ts:178`); no resume id (`agent-session-resume.ts:241`); state: a TypeScript plugin at `~/.config/amp/plugins/orca-agent-status.ts` (`main/amp/agent-status-plugin-source.ts`); `agent.start`/`tool.*` → working, `agent.end` → done (`providers/amp-events.ts:28-33`)                                                                                                 | plugin `~/.config/amp/plugins/cmux-session.ts` (`hooks:80-86`, handlers `CLI/CMUXCLI+AmpExtensionSourcePart3.swift:17-121`); resume `amp threads continue <id>` (`resume:439-442`)            | `amp plugins show-docs`: plugins run under Bun from `~/.config/amp/plugins/*.ts`; events `session.start`/`agent.start`/`tool.call`/`tool.result`/`agent.end{status}`; `tool.call` handlers must return a decision. No permission event                                                                                                             |
| **Pi** 0.85.1               | launch `pi <prompt>`, prefill via extension env (`tui-agent-config.ts:137`); resume `pi --session <transcript>` (`agent-session-resume.ts:266-268`); state: an extension posting `agent_start`/`tool_*`/`agent_end` (`main/pi/agent-status-extension-source.ts`, `providers/pi-family-events.ts:19-47`)                                                                                                                   | extension `~/.pi/agent/extensions/cmux-session.ts` (`hooks:55-61`); resume `pi --session <id>` (`resume:433`)                                                                                 | pi `docs/extensions.md`: `~/.pi/agent/extensions/*.ts`, `pi.on('session_start' \| 'agent_start' \| 'tool_execution_start' \| 'agent_end' \| 'session_shutdown')`; README: "No permission popups". `--session-id <id>` "creating it if missing"                                                                                                     |

### What shipped, and why each is shaped this way

| name           | state wiring (overlay)                                                                  | session id                                             | resume                          | needs_you                                                                        |
| -------------- | --------------------------------------------------------------------------------------- | ------------------------------------------------------ | ------------------------------- | -------------------------------------------------------------------------------- |
| `gemini`       | `harness/gemini/hooks.json` → key-merged into `~/.gemini/settings.json`                 | pre-assigned `--session-id`                            | `--resume <id>`                 | `Notification` (`ToolPermission`), answered by keystroke                         |
| `qwen`         | `harness/qwen/hooks.json` → `~/.qwen/settings.json`; **empty event_map** (Claude table) | pre-assigned `--session-id`                            | `--resume <id>`                 | `PermissionRequest` **held** for `flow.approve`, like Claude Code                |
| `cursor-agent` | `harness/cursor/hooks.json` → merged into `~/.cursor/hooks.json` (flat entries)         | learned (`conversation_id`, bound by `SMOOTH_FLOW_ID`) | `--resume <id>`                 | pane only: no gate hook is subscribed                                            |
| `copilot`      | `harness/copilot/hooks.json` → `~/.copilot/hooks/<pkg>.json` (a file of its own)        | pre-assigned `--session-id`                            | `--resume=<id>`                 | `Notification` with `notification_type` `permission_prompt`/`elicitation_dialog` |
| `droid`        | `harness/droid/hooks.json` → `~/.factory/settings.json`                                 | learned (bound by `SMOOTH_FLOW_ID`)                    | `--resume <id>`                 | `PermissionRequest`, reported not held                                           |
| `amp`          | `harness/amp/plugin.ts` → `~/.config/amp/plugins/<pkg>.ts`                              | learned (thread id, bound by `SMOOTH_FLOW_ID`)         | `threads continue <id>`         | pane only                                                                        |
| `pi`           | `harness/pi/extension.ts` → `~/.pi/agent/extensions/<pkg>.ts`                           | pre-assigned `--session-id` (open-or-create)           | `--session-id <id>` (same argv) | none: Pi has no approvals                                                        |

`th pkg install claude-plugins/smooth-agent --harness gemini,qwen,droid,copilot,amp,pi,cursor`
renders them; `all` includes them. The six new names are **hook-only**
targets: `th pkg` renders their SmoothFlow wiring and nothing else yet (no
skills, MCP or rules). Every merged hook entry and linked file is recorded in
`~/.smooth/pkg/index.toml`, so `th pkg rm` takes back exactly those.

Deliberate choices:

- **Cursor's gate hooks are not subscribed.** `preToolUse`,
  `beforeShellExecution` and `beforeMCPExecution` fail closed on empty
  stdout, and any answer is a decision. orca answers `{"permission":"allow"}`,
  which silently auto-approves every tool call; we don't. `flow-hook.sh`
  prints `{"continue":true}` for `beforeSubmitPrompt` and `{}` for Cursor's
  other hooks, as the first thing it does.
- **Gemini and Copilot get `{}` on stdout** before anything can exit early:
  both parse stdout as JSON. A harness whose stdout was answered never also
  gets a `PermissionRequest` decision printed.
- **Qwen's overlay skips `Notification`.** Qwen sends `idle_prompt` ("Qwen
  Code is waiting for your input") the moment a turn ends, where Claude Code
  waits 60 s, so the Claude table turned every finished turn into needs_you.
  Its asks already arrive as `PermissionRequest`.
- **Copilot's `PermissionRequest` is not subscribed**: it fires before the
  allow/deny rules resolve, and its stdout would be a decision.
- **Amp's `tool.call` is not subscribed**: its handler must return
  allow/reject/modify, and `allow` would step around Amp's permission rules.
  The plugin uses `session.start`, `agent.start` (returns `{}`),
  `tool.result` and `agent.end`.
- **Pi's and Droid's `session_start`/`SessionStart` map to `ignore`**: both
  fire on an idle TUI open (orca: `pi-family-events.ts:19`,
  `droid-events.ts:24`). Mapping them to idle would mark every launch unread.
  So a resumed Pi row reads `starting` until its next turn.
- **Droid hooks go to `~/.factory/settings.json`**, where orca and cmux both
  write them. Factory's docs also accept `~/.factory/hooks.json`.

### Engine changes this needed

- **`SMOOTH_FLOW_ID` in every agent pane**, posted back by `flow-hook.sh` and
  the plugins as `flow_id`. Since th-91d032, the **hook token** names the row
  and `flow_id` is only a consistency check: a `flow_id` that contradicts the
  token's row is refused, and a tokenless `flow_id` binds nothing (anyone can
  claim one). A first hook binds its harness session id only if the posted
  `harness` maps to the row's kind (an agent launched _inside_ a pane inherits
  the env and the token). A row already bound to a different id is left alone
  (a same-kind child), and an ended row accepts nothing. Cursor, Droid and Amp
  can't pre-assign an id; this binds them without guessing by cwd.
- **Only the Claude table holds `PermissionRequest` open.** A harness with its
  own `event_map` (Droid) doesn't speak Claude's decision reply, so its ask
  is reported and the hook returns at once. A mapped **permission** ask gets a
  `request_id` of `hook-<uuid>` (the same rule th-3cabf6 landed), which
  `flow.approve` answers with the approval keystroke, the way a scraped ask is
  answered; a question gets none. The label is derived from the event name or
  `notification_type`, so Gemini and Droid need no `reason` in the payload. Before this, `th flow approve`
  refused it ("no pending permission request"): seen live with Gemini.
- **`map_manifest_event` refines `needs_you`**: an event carrying
  `notification_type` is an ask only when that type is one
  (`*permission*` → permission, `elicitation_dialog` → question, anything else
  → nothing).
- **Steer honors `[steer]`**: the engine pastes, waits `submit_delay_ms`,
  then sends `submit_key`. Before this, `submit_key` was never read. Gemini
  0.59 folds an Enter that arrives 0 or 50 ms after a paste into the composer
  as a newline; 150 ms and 300 ms submit (measured live), so `gemini.toml`
  sets 300.
- **`flow-hook.sh`** reads `session_id` from `.session_id | .sessionId |
.conversation_id` and `cwd` from `.cwd | .workspace_roots[0] | $PWD`.

### Proof — what ran for real

On 2026-09-14, through a private SmoothFlow daemon (the SmoothFlow.app
bundle's `smooth-daemon` 0.48.0; scratch `HOME`, port 8799, tmux socket
`smooth-flow-hooked`, `SMOOTH_ALLOW_SECOND_DAEMON=1`,
`SMOOTH_TAILSCALE_SERVE=0`), with these manifests in the scratch
`~/.smooth/harnesses/`, this branch's overlays installed into the scratch
dotfiles, and models served by `llm.smoo.ai`. Gemini used the gateway's
native Gemini route: `GOOGLE_GEMINI_BASE_URL=https://llm.smoo.ai/gemini`.

| harness          | proven live (every state change came from hooks)                                                                                                                                                                | not proven live                                                                                                                                                                                                    |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `pi` 0.85.1      | launch → working → idle; steer → working → idle; kill+resume relaunched `--session-id <same id>` and the conversation continued ("the first word you asked for was pong"); kill → done                          | — (no approvals to prove)                                                                                                                                                                                          |
| `qwen` 0.23.4    | launch → working → idle; steer; kill+resume `--resume <id>` continued the conversation; `PermissionRequest` → needs_you with a held request id → `th flow approve` → Qwen ran the command with no dialog → idle | Qwen stops waiting on the hook after roughly a minute and shows its own dialog; a later decision doesn't apply (the scrape + keystroke fallback does)                                                              |
| `gemini` 0.59.0  | launch → working → idle; `Notification(ToolPermission)` → needs_you; a raw `1` keystroke into the pane ran the command → working → idle (`th flow approve` refused the ask: no request id, the gap fixed above) | steer submit through the engine: found broken here and fixed with `submit_delay_ms` (the delay itself was measured live, not re-run through a rebuilt daemon); `hook-` request id and `flow_id` bind (unit-tested) |
| `cursor-agent`   | —                                                                                                                                                                                                               | parse/resolve/event tests only. A fresh HOME hits Cursor's browser sign-in, and the real HOME's login is keychain-bound                                                                                            |
| `copilot` 1.0.83 | boot screen captured: folder-trust dialog                                                                                                                                                                       | parse-tested only; stops at GitHub sign-in                                                                                                                                                                         |
| `droid` 0.218.2  | boot screen captured: "Please login with your Factory account"                                                                                                                                                  | parse-tested only; stops at Factory login                                                                                                                                                                          |
| `amp`            | boot screen captured: "Would you like to log in to Amp?"                                                                                                                                                        | parse-tested only; stops at Amp login                                                                                                                                                                              |

No sign-in wall was pressed and no account was created. The CLIs ran from a
scratch npm prefix that was deleted afterwards.

Tests: `harness::tests::hook_capable_builtins_*` (launch/resume argv,
session-id mode, shim-skipping resolution for all seven),
`engine::tests::hook_capable_harness_events_map_to_flow_states` (every native
event each manifest maps, through the engine's own `hook_outcome`, with the
payloads those CLIs send), `hook_auth_refuses_every_forgery`,
`smooth_flow_id_binds_learned_harnesses_and_holds_only_claude_protocol_asks`,
`pkg::tests::hook_only_harnesses_render_and_rm_removes_exactly_that`,
`smooth_agent_flow_overlays_match_the_flow_manifests` (every overlay event is
mapped, every mapped event is subscribed, no Cursor gate hook),
`flow-hook.test.sh`, and `harness/flow-plugins.test.mjs` (Amp/Pi envelopes,
degrade paths).
