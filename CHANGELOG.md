# @smooai/smooth

## 0.9.3

### Patch Changes

- 5e42e47: Fix smooth-dolt subprocesses hanging indefinitely when called from inside Big Smooth's tokio runtime. Root cause: smooth-dolt's Go runtime forks a long-lived `dolt sql-server` child that inherits the parent process's open file descriptors. When `SmoothDolt::run` connected stderr to a pipe (the default behaviour of `Command::output`), the daemon child held that pipe fd open after smooth-dolt itself exited; `Command::output` waited for EOF on the pipe forever. Observed on smoo-hub as `/api/projects` timing out at 60s+ while the same command from a TTY returned in 50ms. Fix is to redirect smooth-dolt stderr to `/dev/null` (`Stdio::null`) so there's no pipe to inherit; on non-zero exit we now surface "rerun the CLI for stderr" instead of the captured message.

## 0.9.2

### Patch Changes

- b38c035: Fix `/api/projects` and `/api/projects/pearls` hanging on Big Smooth when `smooth-dolt` is on slower storage. Both handlers were calling `PearlStore::open` + `store.stats()` / `store.list()` directly inside `async fn` bodies — those functions shell out to the `smooth-dolt` Go binary via blocking `std::process::Command::output`, pinning the tokio worker for the whole subprocess+IPC roundtrip. With multiple registered projects we did N×subprocess sequentially on a single worker, easily blowing past the request timeout (observed: 60s+ on smoo-hub, never returned). Wrapped both handlers in `tokio::task::spawn_blocking` so the work runs on the blocking thread pool and the runtime stays responsive.

## 0.9.1

### Patch Changes

- 783c264: Make `smooth-web` actually usable on phones. Chat now stacks vertically on mobile (single-pane: Chats list when no active chat, Conversation when one is selected, with a back button). The Send button collapses to icon-only under `sm:`. Pearls page now renders an inline project picker (cards, with open/in-progress/closed counts) instead of just printing "Select a project to view pearls" — the existing picker lived in the sidebar drawer which is hidden by default on mobile, so users couldn't find it. Layout `<main>` padding drops from `p-6` to `p-4` on mobile to reclaim ~16px on each side, and chat heights use `100dvh` instead of `100vh` so iOS browser chrome doesn't eat the input row. Inputs all set explicit `font-size: 16px` to prevent iOS Safari's tap-to-zoom behavior.

## 0.9.0

### Minor Changes

- c510661: Rip out the per-language test-output parsers (`parse_pytest`, `parse_cargo_test`). Scoring now runs the language's test command and hands the stdout to the `smooth-judge` routing slot with a strict JSON-only contract — works for pytest, cargo test, go test, jest, gradle, ctest, anything. `parse_judge_response` is unit-tested for code fences, prose-wrapped JSON, partial totals, and malformed output; the LLM call itself is `judge_test_output` and can be called directly by other callers.
- c50cf9e: **TEST phase + self-validating EXECUTE + loop v2.**

  New TEST phase runs AFTER the provided tests pass. Classifies the code (React component / API client / web flow / WebSocket / DB service / CLI / pure library / async code), picks the canonical test stack for that shape (MSW, Playwright, testcontainers, property-based via hypothesis/proptest/fast-check, …), installs missing deps, and writes boundary-pushing tests that exercise real behaviour — not another unit test, but MSW intercepting the actual `fetch` retry loop or a Playwright browser clicking through the actual flow. If its new tests reveal real bugs, the workflow loops back to EXECUTE with them as the next review findings; if they're all green the workflow moves on to FINALIZE. Routed through `smooth-reviewing` (adversarial test writing is closer to code review than fresh implementation). Skippable via `SMOOTH_WORKFLOW_SKIP_TEST=1` for benchmark runs where adding extra tests would change the score.

  EXECUTE prompt now demands the agent pick a **self-validation** check appropriate to the language (`cargo check`, `python -m py_compile`, `go vet`, `node --check`, `tsc --noEmit`, etc.) and run it before declaring done — no more handing off to VERIFY with code that won't compile. Agent-written tests are welcome but MUST land with their implementation in the same change (no orphan failing tests that reference unimplemented methods).

  Loop v2 stop conditions are budget + plateau, not a fixed iteration cap. `verify_signature` extracts pass/fail counts from each VERIFY and breaks early when the signature repeats (model going in circles). Budget short-circuit breaks when the next cycle would likely blow the cap. Default `max_outer_iterations` bumped 3 → 10 as a ceiling, not the governor.

  New thesaurus phrases for the TEST phase — "Writing tests…", "Mocking the network…", "Booting the browser…", "Red-teaming the code…", etc. Status-bar cycle includes them when TEST is active.

- e16232b: **CodingWorkflow** — first real per-phase dispatcher. ASSESS / PLAN / EXECUTE / VERIFY / REVIEW / FINALIZE each run their own `Agent` invocation through a different `Activity` slot: Thinking for ASSESS + FINALIZE, Planning for PLAN, Coding for EXECUTE + VERIFY, Reviewing for REVIEW. Previously Thinking / Planning / Coding / Reviewing were declared-only — no code path routed through them.

  ASSESS now emits a structured `## Goal Summary` section that's threaded through every later phase's user prompt so the agent stays anchored to the objective across review loops. REVIEW can refine the goal summary via an `## Updated Goal Summary` block when it realizes the understanding drifted. FINALIZE checks the final state against the Goal Summary, not just test results.

  Opt-in via `SMOOTH_WORKFLOW=1` in Big Smooth's environment. When set, Big Smooth serializes the `ProviderRegistry` via `ProviderRegistry::to_json` / `from_json` (new) and passes it to the sandboxed runner in `SMOOTH_ROUTING_JSON`. The runner deserializes and dispatches the workflow; otherwise falls back to the existing single-Agent loop.

  `AgentEvent::PhaseStart { phase, alias, upstream, iteration }` emitted at each node entry. TUI listens, tracks `current_phase` / `phrase_idx` in `AppState`, and renders the phase prefix + rotating thesaurus phrase in the status bar:

  ```
  ASSESS · smooth-thinking → kimi-k2-thinking | Pondering… | tokens: 1.2k | spend: $0.003
  ```

  `smooth_code::thesaurus` provides the rotating phrase lists (Pondering… / Hammering… / Nitpicking… per phase). Spinner ticks advance the cycle.

  Companion fixes: `BoardroomNarc` now routes through `Activity::Judge` instead of the Default slot (what the Judge alias was named for), and `ToolRegistry` is `Clone` so multiple phase Agents can share the same tool handles.

- c53943f: Add `th routing resolved` — hits the LiteLLM `/model/info` admin endpoint on each configured provider and prints the alias → concrete-upstream map. Answers "what model actually runs behind `smooth-coding` today?" without needing server-side access. Internally exposed as `smooth_operator::resolution::{fetch_model_info, parse_model_info, ResolvedModel}` so other callers (bench harness, TUI status bar) can reuse it.
- d54cc78: New internal crate `smooai-smooth-bench` — benchmark harness for Aider Polyglot single-task runs. Not part of the user-facing `th` binary; invoke via `cargo run -p smooai-smooth-bench --` or `scripts/bench.sh`. Dispatches to Big Smooth over the headless WebSocket path, runs the language's test command in the scratch work dir, and writes a scored `result.json` to `~/.smooth/bench-runs/<run-id>/`. Parsers for pytest and `cargo test` summaries included; Go / JS / Java / C++ command shapes wired but not exercised yet. SWE-bench, Terminal-Bench, batch mode, and the web scoreboard are separate pearls.
- 422d9a8: Make smooth-web a PWA. Adds `vite-plugin-pwa` with auto-update SW, generated `manifest.webmanifest`, and the new `th` icon as both favicon (16/32 multi-res ICO + PNG variants) and PWA icon set (192/512 + maskable). Adds iOS apple-touch-icon variants (180/167/152/120) and meta tags for Add-to-Home-Screen. The axum static handler now serves `.webmanifest` with the spec'd `application/manifest+json` MIME (mime_guess doesn't know about it).
- 4471d5f: - **Cost threading**: `AgentEvent::Completed` now carries `cost_usd`, and Big Smooth's sandboxed dispatch path forwards that into `ServerEvent::TaskComplete` instead of the hardcoded `0.0` it sent before. `LlmResponse.gateway_cost_usd` captures the authoritative gateway-reported cost (LiteLLM's `x-litellm-response-cost-*` headers, with `-margin-amount` / `-original` / the legacy `-response-cost` all checked); `CostTracker::record_with_cost` replaces local `ModelPricing` guesswork when the gateway reports a real number.
  - **Spend meter in the TUI**: status bar shows `spend: $X.XXX` next to the token count, accumulated from every `ServerEvent::TaskComplete` across the session. Renders `$0` on fresh sessions; three-decimal precision under $1, two-decimal above.
  - **Glob `@` autocomplete**: `@**/*.rs`, `@../**/(dashboard)`, `@~/dev/**/README.md`, `@apps/**/package.json` all resolve through `ignore::WalkBuilder` + `globset`, respecting `.gitignore`. Falls through to the existing path-prefix listing when the query has no glob metacharacters. `(parens)` from Next.js route groups match literally.
- e0f892c: The Line is now visible in two new places:

  - **README badge** — points at `docs/bench-badge.json` (Shields.io endpoint format), auto-updated on every release tag alongside `docs/bench-latest.json`. Thresholds: ≥80% brightgreen, ≥60% yellow, else orange. A partial-sample (budget-cap hit) shows a ⚠ suffix.
  - **`th bench score`** — new subcommand prints The Line baked into this binary at build time. Reads `docs/bench-latest.json` via a `build.rs` rustc-env injection and formats with the same human table `smooth-bench score` uses (shared via `Score::render_table()`). When no Line is baked in yet it prints a hint explaining how to produce one locally.

  Supporting changes: `scripts/the-line/render-badge.sh` (jq-based Shields endpoint renderer), wired into `.github/workflows/the-line.yml` + its dry-run harness. `Score::render_table()` in `smooth-bench` is now public so both the harness binary and the CLI can render identical tables.

- 5f6057c: TUI: `@` now expands paths (`@~/`, `@./`, `@../`, `@/`), mixes pearls into file search results, and `/` triggers anywhere in the input to discover slash commands — not only at input start.
- 59ee646: TUI: redesign `/model` as an activity-slot picker. Top level lists the 8 routing slots (Thinking / Coding / Planning / Reviewing / Judge / Summarize / Default / Fast) with their current model. Enter on a slot opens a sub-picker of candidate models; selecting one applies the routing and persists it to `~/.smooth/providers.json`. Up/Down navigates, Esc backs out (Models → Slots → closed) — previously the picker had no input handling at all and Esc didn't dismiss it.
- 15ef8c5: TUI: add `/rename <title>` to rename the current session from inside the chat, and load pearls in the background so the UI paints immediately instead of waiting for the `smooth-dolt` subprocess to list pearls at startup.

### Patch Changes

- 02aa0c3: Bench: enable-skipped-tests step. Aider Polyglot tasks intentionally ship with most of their tests disabled so the stub code compiles — Rust bowling has 30 of 31 marked `#[ignore]`, JS bowling has 29 `xtest`/`it.skip`/`test.skip`/`xit`/`xdescribe`/`describe.skip` variants. Without flipping these on, the harness scored a "solved" verdict off a single trivial case. Rust now runs `cargo test -- --include-ignored`; JS spec files get their skip markers rewritten (`xtest(` → `test(`, etc.) in the scratch dir before tests run. Source dataset is untouched; only the per-run copy is edited.
- f102738: JS bench command now runs `npm install` before `npm test` — tasks ship only a `package.json` with devDependencies (jest/babel), no `node_modules`. Java bench uses the bundled `./gradlew --no-daemon` wrapper so version drift between the task and the sandbox doesn't matter.
- 76bb2a1: Java skip-strip. Polyglot Java tasks ship with `@Disabled` / `@Ignore` on 30-of-31 tests (same pattern as Rust `#[ignore]` and JS `xtest`/`test.skip`). Without the strip, a Java bowling run scored 1/32. Harness now rewrites `@Disabled` / `@Disabled("reason")` / `@Ignore` / `@Ignore("reason")` annotations out of test files in the scratch work dir (only test files, not production code — avoids clobbering unrelated annotations like `@DisabledInNativeImage`).
- 503a590: Bench: strip agent-added test files before scoring. Polyglot scorer runs the test command over the whole work dir, so any `test_*.py` / `*_test.go` / `*.spec.ts` / `*Test.java` / etc. the agent added during EXECUTE would get counted and tilt the score. The harness now snapshots the original file set before dispatching to the agent, and after the run deletes any files that (a) weren't in the snapshot AND (b) match per-language test-file conventions. Non-test files the agent added (new helpers, modules) are left alone. Original test files are always preserved. Benchmark invariant: only the provided tests count.
- c50cf9e: CodingWorkflow loop v2: stop conditions are budget + plateau, not a fixed iteration cap. Default `max_outer_iterations` bumped 3 → 10; the real governor is `verify_signature`, which extracts pass/fail counts from each VERIFY and breaks early when the signature repeats (model going in circles). Budget short-circuit added too — if next iteration would likely blow the cap, break. `verify_signature` is unit-tested across pytest/cargo/go/jest summaries, compile-error lines, and progress deltas.
- 4a2ff1a: Bench: judge prompt and test commands tightened for suite-level summaries. Drop `cargo test --quiet` and add `-v` to `go test` so each runner emits per-case lines. Judge system prompt now has explicit scoring rules — `ok <package>` with no per-case detail maps to passed=1/total=1 instead of returning all zeros, which previously marked a passing Go suite as UNSOLVED. Build errors count as failed=1.
- 34d9d7a: Repo-aware EXECUTE + TEST phases. Prompts now instruct the agent to inspect the repo first — `package.json` scripts / `Cargo.toml` / `pyproject.toml` / `go.mod` / `Makefile` / `.github/workflows/` — and pick validation + testing tools that match what the project already uses. Generic defaults (`cargo check`, `py_compile`, `go vet`, MSW, Playwright) are fallbacks only; the TEST phase won't suggest Playwright for a pure CLI or MSW for a Rust crate. README overhauled with the new 7-phase workflow diagram (ELK renderer, orthogonal 90° lines) and the per-phase routing table.
- 6f5fc06: Tool-call wire format is now strictly canonical: `function.arguments` always serializes to a JSON-object string, never `"null"` or a primitive. Strict providers (qwen3-coder-plus on DashScope) reject anything else with `InternalError.Algo.InvalidParameter: The "function.arguments" parameter of the code model must be in JSON format.` Fix lives in `canonical_tool_arguments_json`; also replaces the streaming-parse fallback from `Value::Null` to `Value::Object(empty)` so malformed deltas don't poison the next-turn echo. New `smooth_operator::quirks` module is the home for future per-upstream-model tweaks — seeded with qwen3 / qwen-coder flags, otherwise empty.

## 0.8.0

### Minor Changes

- 02fd111: TUI autocomplete: `@` for file paths, `/` for slash commands.

  The file-reference autocomplete state has always existed in
  `smooth-code` but was never wired into the event loop or
  rendered. Now it is, plus a parallel slash-command flow.

  - **`@`** anywhere in the input box pops the file picker with every
    entry in the workspace file tree. Type to narrow by
    case-insensitive substring on filename.
  - **`/`** at the start of the input pops the slash-command picker
    listing every registered command (`/help`, `/clear`, `/model`,
    `/save`, `/sessions`, `/quit`, `/status`, `/compact`, `/diff`,
    `/tree`, `/fork`, `/goto`, …) with one-line descriptions. Type to
    narrow by case-insensitive prefix.
  - **Up/Down** arrows move the selection, **Tab** or **Enter**
    accepts, **Esc** closes the popup, typing a space ends the active
    query. Backspace past the trigger char closes it.
  - Popup is a floating overlay anchored just above the input box, so
    the eye doesn't jump far from where you're typing. Orange border +
    "▶ " marker on the selected row to match the rest of the brand.
  - New types: `CompletionKind { File, Command }`, detail line on
    `AutocompleteResult`, `trigger_pos` on `AutocompleteState`.
  - New methods: `activate_commands`, `update_command_query`,
    plus two regression tests.

## 0.7.1

### Patch Changes

- a4a5063: TUI colors: orange is the primary frame accent, green is a
  secondary accent on assistant labels and the banner gradient.
  Previously panel borders + the chat title used green, which made
  the input-box border blend with assistant labels — users reported
  they couldn't see where to type.

  - `panel_border(true)` → Smoo AI orange (`#f49f0a`), was green.
  - `title()` → orange, was green.
  - New `input_border(mode)` helper: the message-input panel gets an
    orange bold border in input mode and a gray border only when the
    user explicitly escapes into normal mode. The chat panel follows
    focus; the input panel stays obvious as "the place to type."
  - New "▶ Message" title on the input panel, orange + bold.
  - Assistant labels stay green (secondary accent), user labels stay
    orange (primary accent), banner keeps the orange→green vertical
    gradient — green lives on as the destination color.

  All colors verified against
  `smooai/packages/ui/globals.css` (the canonical palette): orange
  `#f49f0a`, green `#00a6a6`, red `#ff6b6c`, blue `#bbdef0`,
  gray-700 `#4e4e4e` all match.

  Regression test: `test_input_border_is_orange_in_input_mode_gray_in_normal`.

- f8f3ed3: TUI: remove CSI 2026 synchronized-output wrapper around each
  render. Fixes the "I can type but the screen doesn't update until I
  ^C" class of bug reported on at least one macOS terminal.

  Root cause: the event loop wrapped each `terminal.draw` with
  `print!("{}", begin_sync())` / `print!("{}", end_sync())`. On
  terminals that half-support CSI 2026 (or where `print!` doesn't
  flush between the begin and the end), frames get stuck in the
  terminal's buffer until the process exits and stdout flushes on
  teardown — so typed input appears to be ignored until you kill
  `th`.

  ratatui's backend already produces flicker-free output via
  crossterm's diff-based rendering, so the sync wrapper was a
  micro-optimization not worth the fragility it introduced. Dropped.

## 0.7.0

### Minor Changes

- 18e1398: `th code --resume` and auto-generated session titles.

  - **Session titles.** `Session` now carries an optional
    `title: Option<String>`. The TUI's input handler detects the first
    user message, spawns a detached `smooth-fast` call to generate a
    3–6 word Title Case summary, and stores it on `AppState`. Chat
    latency isn't gated on the name. Previously saved sessions without
    titles still load — `SessionSummary::display_label()` falls back
    to the message preview.
  - **`th code --resume [query]`**. New CLI flag. Resolution tiers:
    exact id → unique id prefix → unique title substring
    (case-insensitive) → unique preview substring. No argument picks
    the most recently updated session. Ambiguous matches error with
    the candidate list. Reuses the same auto-naming pipeline as the
    web chat so titles are consistent across TUI + web.
  - **`th code --list`**. Prints saved sessions newest first with
    display label, short id, and updated time, then exits without
    launching the TUI.
  - `AppState::from_resumed_session()` + `app::run_with_session()`
    restore a persisted session as the starting state. The welcome
    message is suppressed on resume in favor of a "Resumed session: &lt;title&gt;"
    marker.
  - Six regression tests on `SessionManager::find_by_query` +
    `most_recent` covering each tier and the ambiguous-match path.

## 0.6.0

### Minor Changes

- cf42e73: New `Activity::Fast` routing slot + LLM-generated session titles.

  **`smooth-fast` slot**: a new utility routing slot for short,
  latency-sensitive calls — session naming, short titles, autocomplete,
  one-liner tool-result summaries. Targets a Haiku-class model. The
  llm.smoo.ai gateway exposes it as `smooth-fast` (anthropic Haiku 4.5
  behind the alias).

  - `Activity::Fast` variant added to the routing enum.
  - `ModelRouting.fast: Option<ModelSlot>` — optional on disk, so
    existing `providers.json` files still parse. When absent,
    `slot_for(Activity::Fast)` falls back to the `default` slot.
  - Every preset (`SmoaiGateway`, `OpenRouterLowCost`, `LlmGatewayLowCost`,
    `OpenAI`, `Anthropic`) now configures a sensible `fast` slot
    (Haiku / gpt-4o-mini / gemini-flash-lite).
  - `th routing show` lists the new slot so users can see where
    utility calls go.

  **Session auto-naming**: first-message titles now come from the
  `smooth-fast` slot — 3–6 words, Title Case, trimmed — instead of a
  60-char truncation of the user's prompt. The LLM call is spawned
  into a detached tokio task so chat response latency is unaffected.
  On LLM failure we fall back to the legacy truncation so a session
  is never stuck at "New chat".

  Wire it up in your `~/.smooth/providers.json` once llm.smoo.ai's
  `smooth-fast` alias is live in prod:

  ```json
  "routing": {
    …existing slots…,
    "fast": { "provider": "smooth", "model": "smooth-fast" }
  }
  ```

## 0.5.5

### Patch Changes

- 6f9b259: TUI: drop "Coding", use the brand gradient for the wordmark.

  - "Smooth Coding" → "Smooth" everywhere user-visible (chat panel
    title, welcome message, doc strings). The product's name is
    "Smooth" — this is the coding surface of it, not a separate
    product.
  - New `theme::smooth_wordmark()` returns a `Vec<Span<'static>>`
    rendering "Smooth" with the same per-character gradient the CLI
    uses in `gradient::smooth()`:
    - `Smoo` → #f49f0a orange → #ff6b6c pink (linear over 4 chars)
    - `th` → #00a6a6 teal → #1238dd blue (linear over 2 chars)
      The chat panel border title now uses it, so the wordmark in the
      TUI matches the `th` CLI banner and the horizontal logo.

## 0.5.4

### Patch Changes

- 4abae68: th-324b12: instrument `smooth-code`'s TUI startup so "my terminal
  shows nothing" is diagnosable without a screen recording.

  Three additions to `crates/smooth-code/src/app.rs::run`:

  1. **TTY pre-flight.** If `stdin` or `stdout` isn't a terminal, fail
     with a clear message pointing at `th code --headless`. Previously
     the app would enter alt-screen, render to /dev/null, and exit
     cleanly — the user saw nothing and had no clue why. Also reliably
     caught via a regression test (`run_requires_tty`).

  2. **`SMOOTH_TUI_NO_ALT_SCREEN=1` escape hatch.** Some terminals
     (a few tmux configs, certain Windows terminals, odd ssh
     multiplexes) don't cleanly combine alt-screen + mouse-capture +
     CSI 2026 synchronized output. The env var drops alt-screen and
     mouse-capture so the UI renders inline in the primary buffer —
     scrollback gets mixed with the TUI output but at least the user
     can _see_ something.

  3. **`SMOOTH_TUI_DEBUG=1` step log.** When set, every major startup
     step (TTY check, raw-mode enable, alt-screen enter, Terminal
     creation + size, first draw, event-loop entry + exit, terminal
     restore) logs to `~/.smooth/logs/smooth-code.log` with a
     timestamp. Zero-cost when unset. Lets us trace exactly where
     `run()` gave up on an environment-specific blackout without
     needing a tmux capture.

  Also: initial forced `terminal.draw` before the event loop starts,
  so even if `event::poll` blocks for a long time on the first
  iteration, the welcome message is visible immediately. Previously
  the draw only happened at the top of the loop body, gated by the
  auto-save check — a startup stall could delay the first frame.

  Improved error messages on `enable_raw_mode` + `EnterAlternateScreen`
  failures suggest `SMOOTH_TUI_NO_ALT_SCREEN=1` as the first thing to
  try when a terminal silently rejects the setup.

## 0.5.3

### Patch Changes

- a85ea2b: Fix "Dolt store" showing red on the dashboard while green on
  `th status`. Pre-existing pearl stores were created before the
  `config` table was part of the schema (added in the retire-sqlite
  change), and only `PearlStore::init` ran `ensure_schema`. `open()`
  skipped it entirely, so `get_config("__health_check")` in the health
  handler ran `SELECT v FROM config WHERE k = ...` against a missing
  table, failed, and flipped `database.status` to `"down"`.

  `PearlStore::open` now runs an idempotent schema-migration check: a
  single `SHOW TABLES` query against the open store; if any required
  later-added table is missing, it re-runs the full `CREATE IF NOT
EXISTS` pass and commits. On an up-to-date store it's a single
  round-trip. Concurrent migrators are safe — duplicate commits are
  logged and swallowed.

  Added a regression test
  (`test_open_migrates_missing_config_table`) that simulates a legacy
  store by dropping `config`, reopens via `open()`, and verifies
  `get_config` / `set_config` work without error.

- 82cca37: Remove the `images` job from the release workflow until we fix
  smooth-dolt's aarch64-linux-musl cross-compile.

  Current state:

  - `ghcr.io/smooai/smooth-operator:0.2.0` / `:latest` and
    `ghcr.io/smooai/boardroom:0.2.0` / `:latest` are already public on
    GHCR (pushed manually the day we went public). Smooth pulls `:latest`
    by default so end users are unaffected.
  - The `images` job was green through docker login after the `GH_PAT`
    scope fix, but then failed on `build-boardroom.sh` — that script
    expects a cross-compiled `smooth-dolt` at
    `target/aarch64-unknown-linux-musl/release/smooth-dolt`, which
    nothing currently produces. `build-smooth-dolt.sh` is a host-arch
    `go build` that lands at `target/release/` (glibc-linked), so the
    alpine-based boardroom image can't copy it.

  Options for the follow-up (tracked in a pearl):

  1. Switch the boardroom image base from alpine to
     `debian:slim-aarch64` so a host-linked smooth-dolt runs natively.
  2. Cross-compile smooth-dolt to aarch64-musl using `zig cc` as the Go
     CGO compiler (the same zigbuild workflow Rust uses).
  3. Build smooth-dolt inside a containerized alpine stage during
     `docker build` and COPY the result.

  Until then, image pushes are manual via
  `scripts/build-smooth-operator-image.sh --push` and
  `scripts/build-boardroom-image.sh --push`.

- 83ba4d1: Fix **th-dfd0d3**: every sandboxed tool call was being rejected with
  "error decoding response body" because `WonkHook` inside the operator
  runner never carried the per-VM bearer token. The same security
  hardening commit (`f7676d8`) that added `Authorization: Bearer` auth
  to Wonk's `/check/*` endpoints updated `WonkClient` (used by Goalie)
  but left `WonkHook` (used by the agent's tool registry) untouched.
  Every `pre_call` → `/check/tool` now gets a 401 with an empty body,
  and `resp.json::<CheckResponse>()` surfaces as the opaque
  "error decoding response body" at the hook layer.

  Changes:

  - `WonkHook::with_auth(url, token)` constructor; `new` remains as
    a zero-token shim for legacy tests.
  - Per-request `Authorization: Bearer <token>` when the token is
    non-empty.
  - `check()` now inspects HTTP status before attempting to decode as
    JSON — on a non-success response we surface
    `"Wonk /check/... returned 401: <body>"` instead of the misleading
    decode error. Future misconfigurations will be obvious.
  - `smooth-operator-runner` stores the operator token on `Cast` and
    wires `WonkHook::with_auth(&cast.wonk_url, &cast.operator_token)`
    into the tool registry.
  - Regression tests on `WonkHook` pre-call:
    `pre_call_without_token_surfaces_401_not_decode_error` (negative)
    and `pre_call_with_auth_passes_through` (positive).

  Also fixed a CI-flaky test on the side: the two
  `smooai_gateway_*` provider tests both mutate the global
  `SMOOAI_GATEWAY_URL` env var and ran in parallel, racing each other.
  Added a module-local `Mutex` so they serialize.

## 0.5.2

### Patch Changes

- fac7b49: Add grandiose READMEs to the 8 crates that didn't have one: `smooth-narc`,
  `smooth-scribe`, `smooth-plugin`, `smooth-goalie`, `smooth-diver`,
  `smooth-archivist`, `smooth-wonk`, `smooth-bootstrap-bill`.

  Each README follows the cast-lore voice of the main repo — centered
  banner, tagline that names the character's role, badges, one-paragraph
  "why this exists", key types, and a minimal usage example. All eight
  now render proper marketing-quality pages on crates.io rather than the
  blank no-README placeholder.

  `readme = "README.md"` added to each Cargo.toml so the file lands in
  the published crate tarball.

## 0.5.1

### Patch Changes

- ba89132: GHCR images job: log in with `GH_PAT` instead of the default
  `GITHUB_TOKEN`. The initial image pushes were done from a local
  docker login, so the packages are tied to the user account rather
  than the workflow — GITHUB_TOKEN hits `denied: permission_denied:
write_package` on subsequent CI pushes. `GH_PAT` has write:packages
  on the SmooAI org so the workflow can keep updating the existing
  packages.
- f2bb6ad: Release workflow fixes after the first `pnpm ci:publish` run:

  - **Crates.io new-crate rate limit.** Publishing 8 never-before-seen
    crates in a row tripped crates.io's new-crate rate limit on
    `smooai-smooth-diver`. `ci-publish.mjs` now sleeps 15s between
    publishes when the previous one was a first-ever upload. Version
    bumps of already-published crates publish back-to-back as before
    (that limit is far more generous).

  - **GHCR image job zigbuild deps.** The OCI-image job called
    `scripts/build-operator-runner.sh` which requires `cargo-zigbuild`
    - `ziglang`. Now installed explicitly in the job. Also added
      `libicu-dev` + `setup-go` for `smooth-dolt`, which the boardroom
      image bundles.

## 0.5.0

### Minor Changes

- 1debf51: Go public: auto-publish Rust crates to crates.io and OCI images to
  GHCR on every release.

  - **Crates.io**: 11 library crates (`smooai-smooth-policy`, `-operator`,
    `-bootstrap-bill`, `-pearls`, `-narc`, `-scribe`, `-plugin`, `-goalie`,
    `-diver`, `-archivist`, `-wonk`) now publish in dependency-topological
    order via `pnpm ci:publish` on version-PR merge. Idempotent — re-runs
    skip crates whose target version already exists on the sparse index.
    `smooth-web` / `smooth-bigsmooth` / `smooth-code` / `smooth-cli` /
    `smooth-operator-runner` are marked `publish = false` for now; the
    first three need a web/dist include fix, the binaries ship as tarballs.

  - **GHCR**: `smooai/smooth-operator` and `smooai/boardroom` images are
    built on `ubuntu-24.04-arm` (native linux/arm64, avoiding qemu
    emulation) and pushed to `ghcr.io/smooai/*` with both the release
    version tag and `:latest`. Uses the Actions-default `GITHUB_TOKEN`
    (has `write:packages` scope automatically).

  - **sync-versions.mjs fixes**: the old script regex matched `smooth-*`
    when everything was renamed to `smooai-smooth-*` in commit `933b927`,
    so Cargo.lock was silently never updated. Workspace.dependencies
    smooth-X entries had hand-maintained version fields (some pinned to
    `0.2.0`, most missing entirely). Now every entry gets a synced
    `version = "x.y.z"` automatically.

  - **ci:version vs ci:publish**: `changesets/action` was running the
    default `changeset version` directly, so Cargo.toml + Cargo.lock
    bumps happened only in the downstream `publish` step — too late for
    the version PR. Split into `pnpm ci:version` (changesets/action's
    new `version` input) and `pnpm ci:publish` (still the post-merge
    `publish` input, now actually publishes crates).

  New secret required: `SMOOAI_CARGO_REGISTRY_TOKEN` in the repo's
  Actions secrets (scope: read + publish for the `smooai-smooth-*`
  prefix). GITHUB_TOKEN covers GHCR pushes automatically.

## 0.4.4

### Patch Changes

- 1c7597a: Release workflow: build `aarch64-unknown-linux-gnu` on a native ARM
  runner instead of cross-compiling from x86_64.

  Cross-compilation was failing at `pkg_config failed: pkg-config has
not been configured to support cross-compilation` — libdbus-sys's
  build script needs per-architecture pkg-config sysroot + prefix vars,
  which are annoying to set correctly and fragile across dep updates.

  `ubuntu-24.04-arm` is now a free GitHub-hosted runner, so we switch
  the aarch64 Linux matrix entry to it. That makes the build a plain
  native build: same `libdbus-1-dev` + `libcap-ng-dev` apt deps, no
  multi-arch, no cross linker env, no sysroot juggling.

  Also removed the now-unused `gcc-aarch64-linux-gnu` install step and
  the `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` env override.

## 0.4.3

### Patch Changes

- 94ca7d2: Release workflow: add `libcap-ng-dev` to the Linux runner deps.

  After `libdbus-1-dev` unblocked compilation, the link step failed with
  `rust-lld: error: unable to find library -lcap-ng` on both Linux
  targets. `microsandbox`'s Linux-only `msb_krun_devices` uses libcap-ng
  for CAP\_\* capability management in the VM host shim, so the headers
  need to be present at link time.

## 0.4.2

### Patch Changes

- 272e90f: Release workflow: install `libdbus-1-dev` on Linux runners.

  `libdbus-sys` (pulled in transitively via the keyring / zbus chain
  that microsandbox depends on) runs `pkg-config` at build time and
  fails with "pkg_config failed" if the dev headers are missing. Both
  `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` jobs were
  failing there.

  Also separated the cross-compile toolchain install (aarch64 only)
  from the common Linux build deps step.

## 0.4.1

### Patch Changes

- 38bfd54: Release workflow: drop `x86_64-apple-darwin` from the build matrix
  and set `fail-fast: false`.

  Intel macOS has been blocking every release since microsandbox was
  wired into smooth-bigsmooth: the upstream `msb_krun_utils` v0.1.9
  crate references `kvm_bindings::kvm_irq_routing_entry` without a
  `cfg(target_os = "linux")` guard, so it fails to compile on any
  non-Linux target. On Apple Silicon the build never gets that far
  because different HVF code paths are used, but Intel macOS hits the
  wall every time.

  Until upstream gates that type properly, we ship:

  - `aarch64-apple-darwin`
  - `x86_64-unknown-linux-gnu`
  - `aarch64-unknown-linux-gnu`

  `fail-fast: false` also means a future single-platform regression
  won't silently cancel sibling builds, so we can ship the remaining
  targets while we fix the broken one.

## 0.4.0

### Minor Changes

- e7e533e: MCP servers, CLI-wrapper plugins, and project-scoped config.

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

- 3657db9: Chat gets sessions. The smooth-web chat page now lists prior
  sessions in a sidebar, each persisting its own message history in
  the Dolt `session_messages` table. New `/api/chat/sessions`
  endpoints (create/list/get/delete + messages). The LLM receives the
  last 50 messages on every turn so multi-turn context is preserved.
  Session titles auto-rename to the first 60 chars of the opening
  prompt. Chat layout fixed so the input row sits flush to the
  viewport bottom (no stray scroll).
- 3657db9: Retire SQLite. Pearls, sessions, memories, config, and worker metadata
  all live in the Dolt store at `~/.smooth/dolt/` (home) or
  `<repo>/.smooth/dolt/` (per-project). `smooth.db` is gone; the
  dashboard reads "Dolt store (pearls + config)" instead of
  "Database (SQLite)". `th pearls migrate-from-sqlite` removed —
  transitional tool, no longer needed.
- 3657db9: Run in sandbox — the agent does its work in a microVM, you review it live.

  - `smooai/smooth-operator` image (unified — agent installs toolchains at
    runtime via `mise`; covers node/python/rust/go/bun/deno/~140 more).
  - `th run [pearl-id] [--keep-alive] [--image ...] [--memory-mb N]` —
    dispatches via `/api/tasks`, streams SSE events, optionally keeps the
    VM alive for dev-server review.
  - `th operators list / kill <id>` — see and tear down running VMs.
  - `th cache list / prune / path / clear` — project-scoped sandbox
    cache at `~/.smooth/project-cache/<name>-<hash>/`, bind-mounted
    at `/opt/smooth/cache` inside the VM. LRU prune by mtime.
  - Auto-forward common dev-server ports (3000, 3001, 4000, 4200, 5000,
    5173, 8000, 8080, 8888) on keep-alive runs; print reachable
    `http://localhost:<host>` URLs after the agent completes.
  - Per-run memory override threaded through
    `TaskRequest → SandboxConfig`.

- 4f91014: `th service` — background service wrapper for `th up`.

  User-level install by default on all three platforms:

  - **macOS**: writes `~/Library/LaunchAgents/com.smooai.smooth.plist`,
    drives `launchctl bootstrap gui/$UID`.
  - **Linux**: writes `~/.config/systemd/user/smooth.service`, drives
    `systemctl --user enable --now`. Prints a hint to run
    `loginctl enable-linger` so the service survives logout.
  - **Windows**: creates a logon-triggered Scheduled Task via `schtasks`.

  Commands: `install [--system]`, `uninstall`, `start`, `stop`, `restart`,
  `status`, `logs [-f]`. `--system` prints the system-level artifact +
  install instructions to stdout instead of running under sudo.

  Logs stream to `~/.smooth/service.log` and `~/.smooth/service.err`.

### Patch Changes

- 11f0c00: Fix 5 cast_integration tests that had been failing in CI since the
  Wonk bearer-token auth was added in `f7676d8`. The release workflow
  has been red for ~8 days, stranding 12 changesets and blocking every
  version bump.

  Root cause: `ALLOW_EXAMPLE_POLICY` has `[auth] token = "test-token"`,
  so Wonk's `require_operator_token` middleware rejects any request
  without `Authorization: Bearer test-token` with a 401 (empty body).
  The tests built `reqwest::Client::new()` directly and called
  `.post(...).json(...).send().await.unwrap().json().await.unwrap()`,
  which panicked at the final `.json()` with
  `reqwest::Error { kind: Decode, source: Error("EOF while parsing a value") }`.

  Fix: introduce `TEST_AUTH_TOKEN = "test-token"` next to the policy
  fixture, attach `.bearer_auth(TEST_AUTH_TOKEN)` to every direct Wonk
  request, and switch `spawn_goalie` to `WonkClient::with_auth` so its
  `/check/*` calls carry the header too. The `goalie_forwards_..._for_allowed_request`
  test had surfaced as a `403 != 200` assertion for the same reason —
  Goalie was failing its auth to Wonk and correctly denying the request.

  Narc / Scribe / Archivist tests were never affected (those services
  do not require auth).

- 3b1a88a: Diagnostic logging on the sandboxed dispatch path so we can tell
  _why_ `th run` / `th code --headless` fail when they do:

  - `bill::exec_sandbox` logs exec start + non-zero-exit with
    captured stdout/stderr tails (was silent before, making code=-1
    failures opaque).
  - Dispatch handler now runs a preflight `/bin/sh` probe against
    the sandbox before exec-ing the runner — surfaces whether
    bind-mounts landed, whether the runner binary is visible + executable,
    and what the guest's `/opt` actually contains.

  Pearl `th-1ec3ce` (P0) tracks the underlying issue: on plain alpine,
  microsandbox's bind-mounts aren't reaching the guest at all, so
  every sandboxed dispatch fails with `exit=-1 stderr=""`. Fix requires
  digging into microsandbox's mount-arg plumbing; these changes just
  give us the visibility to do it.

- 72f7eef: Fix P0 (th-1ec3ce): sandbox bind-mounts not landing in the guest VM.

  The microsandbox guest agent does not `mkdir -p` mount targets before
  calling `mount -t virtiofs` — mounts to paths that don't pre-exist in
  the rootfs (`/opt/smooth/bin`, `/opt/smooth/policy`, `/workspace`)
  silently fail. We were falling back to plain `alpine` because our
  custom `smooth-operator` image was only in Docker Desktop's local
  store and microsandbox couldn't pull it; alpine has an empty `/opt`,
  so every mount missed.

  Fix: publish `smooai/smooth-operator` and `smooai/boardroom` images
  to GitHub Container Registry (public), and default to pulling from
  there. The `Dockerfile.smooth-operator` pre-creates `/workspace`,
  `/opt/smooth/bin`, and `/opt/smooth/cache/mise` — so every bind-mount
  target now exists before the guest agent tries to mount on top of it.

  - `SandboxConfig` default image: `alpine` → `ghcr.io/smooai/smooth-operator:latest`
  - `th run` default: `smooai/smooth-operator:latest` → `ghcr.io/smooai/smooth-operator:latest`
  - `scripts/build-smooth-operator-image.sh` + `build-boardroom-image.sh`:
    default `SMOOTH_IMAGE_REPO` to `ghcr.io/smooai/...`, add `--push`
    flag so one command builds + publishes.
  - Preflight probe now confirms mounts land: `/opt/smooth/bin/smooth-operator-runner`
    is executable inside the VM and the runner boots as expected.

  Users can override with `SMOOTH_WORKER_IMAGE` / `SMOOTH_OPERATOR_IMAGE`
  if they publish a fork to a different registry. Public pulls from
  `ghcr.io/smooai/*` require no auth.

- 3657db9: Buildable OCI images for the microVM cast:

  - `docker/Dockerfile.smooth-operator` + `scripts/build-smooth-operator-image.sh`
    → `smooai/smooth-operator:<version>` (alpine + mise + runner).
  - `docker/Dockerfile.boardroom` + `scripts/build-boardroom-image.sh`
    → `smooai/boardroom:<version>` (alpine + boardroom bin + smooth-dolt).
  - Both scripts delegate to the existing cross-compile flow
    (`build-operator-runner.sh` / `build-boardroom.sh`).
  - Fixed a latent package-name bug in `build-boardroom.sh`
    (`-p smooth-bigsmooth` → `smooai-smooth-bigsmooth`).

  Still pending: registry publish on release so `microsandbox` can
  pull without Docker on end-user machines.

- 3657db9: Pearl fixes:

  - `/api/pearls` + `/api/projects/pearls` default to unbounded
    (`?limit=0`). The dashboard was silently capped at 100 — a repo
    with 150+ pearls showed "100 closed, 0 open" when the open ones
    were past the limit. LLM tool callers still get a 100-row
    default via `list_pearls()`.
  - `PearlStore::close` is now invoked on every error-path exit of
    the sandboxed dispatch handler (runner not found, workspace
    create failed, LLM provider missing, runner exited non-zero).
    Previously only exit-0 closed the pearl; leaked `Task: ...`
    pearls accumulated from E2E runs.
  - `install:th` now re-adhoc-signs a neighbor `smooth-dolt` binary
    in `~/.cargo/bin/` so `scp`'d copies work under `launchd`
    without a manual `codesign`.

- 3657db9: `th doctor --init-home-repo` scaffolds `~/.smooth/` as a git repo for
  backup / cross-machine sync. Writes a `.gitignore` that excludes
  secrets (`providers.json`), service logs, audit logs, the Dolt
  store (has its own push/pull), the project cache, and ephemeral
  debug captures. Idempotent. Optional `--remote <url>` adds origin.
- 3657db9: Wordmark + cast renames in user-facing surfaces:

  - `th` CLI + web dashboard now say "Big Smooth" (not "Leader") and
    "Smooth Operators" (not "Sandbox").
  - New horizontal logo (`images/logo.png`, `crates/smooth-web/web/public/logo.svg`)
    replaces the old mark.
  - `th up` / `th status` / `th doctor` banners render "Smooth" with
    the logo's gradient colors via ANSI 24-bit truecolor escapes
    ("Smoo" orange→pink, "th" teal→blue).
  - `/health` service field renamed `smooth-leader` → `big-smooth`.
  - `SMOOTH_SANDBOX_MAX_CONCURRENCY` env + `th up --max-operators N`
    flag expose the previously hardcoded pool cap of 3.

- 3b1a88a: Wordmark gradient (`"Smoo"` orange→pink, `"th"` teal→blue) live in
  `th up` / `th status` / `th doctor` banners via a new
  `gradient` module in smooth-cli. Helper `smooth()` stitches the
  two halves into the full word; `smoo_ai()` covers the Smoo AI
  brand. Pure ANSI 24-bit truecolor, no new deps. 4 unit tests.

## 0.3.0

### Minor Changes

- 799c5ca: Major session: Changesets CI, web dashboard, provider overhaul, port forwarding, security enforcement.

  - Changesets versioning with auto Cargo.toml sync and GitHub Actions release pipeline
  - Husky git hooks with pre-commit cargo checks
  - th up daemon mode (background process with pid file), th down kills it
  - Interactive th auth login with provider/model picker and connection test
  - Kimi + Kimi Code providers (OpenAI-compat and Anthropic-compat)
  - Removed deprecated OpenCode Zen module entirely
  - Web dashboard: shadcn UI components, Tailwind v4 dark theme, responsive sidebar
  - Multi-project pearl support: /api/projects, project switcher, per-project pearl views
  - Pearl kanban with search, timeline view, stats view with bar charts
  - System topology SVG graph (radial layout, pulsing nodes, auto-refresh)
  - Clickable dashboard cards navigating to system/pearls
  - Port forwarding Phase 1: PortPolicy, forward_port tool, Wonk /check/port
  - VM path mapping: guest→host translation for filesystem deny pattern enforcement
  - Rich ANSI colors across CLI (th up/down/status/auth/doctor)
  - Doctor checks on th code startup

## 0.2.1

### Patch Changes

- a213b90: Remove deprecated OpenCode Zen module and all references. Add Kimi Code as a provider. Chat handler now uses ProviderRegistry instead of hardcoded OpenCode Zen API.

## 0.2.0

### Minor Changes

- 5b31872: Add Changesets versioning with automated version sync to Cargo workspace. Includes sync-versions script, CI publish workflow, and multi-platform binary release on version bump.
