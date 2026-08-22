# Bench Harness

#engineering

> [!info] How we measure ourselves
> `crates/smooth-bench/` scores the agent four ways — can it edit code,
> does it take the right actions, is it any good to talk to, and do all
> five engines still work. "The Line" tracks the coding score over time;
> the model leaderboard ranks models against each other.

> [!warning] There is no `th bench`
> `smooth-bench` is a separate internal binary, deliberately **not**
> shipped in the `th` CLI. This page used to say `th bench` and to tell
> you to build `smooth-operative` — a binary removed with the microVM
> stack ([[../Decisions/ADR-004-remove-microvm-sandbox-stack]]). Both
> were wrong; run the commands below.

## The suites

| Command          | Question it answers                                                                             |
| ---------------- | ----------------------------------------------------------------------------------------------- |
| `aider-polyglot` | Can it edit code? One curated Exercism-style task, scored by running the canonical test suite.  |
| `score`          | Engine parity — the curated suite through each of the five smooth-operator engines.             |
| `agentic`        | Does it take the right ACTIONS? Seeds a workspace, drives one turn, scores the resulting state. |
| `convo`          | Is it any good to talk to? Multi-turn, LLM driver + LLM judge.                                  |

Scenario corpora live beside the crate: `agentic-scenarios.toml` (the
general suite), `frontend-scenarios.toml` (modern-stack API currency) and
`greenfield-scenarios.toml` (from-nothing builds). `curated-tasks.toml`
holds the coding tasks.

## Running locally

```bash
# every command is `cargo run -p smooai-smooth-bench -- <suite>`
cargo run -p smooai-smooth-bench -- agentic --only no-over-refusal
cargo run -p smooai-smooth-bench -- convo   --model deepseek-v4-flash
cargo run -p smooai-smooth-bench -- agentic --scenarios crates/smooth-bench/frontend-scenarios.toml
cargo run -p smooai-smooth-bench -- score   --engine rust --engine go

# every suite: --help lists the flags
cargo run -p smooai-smooth-bench -- agentic --help
```

Credentials come from `SMOOAI_GATEWAY_KEY`, falling back to the
`llm.smoo.ai` provider in `~/.smooth/providers.json` — the same store the
daemon reads, so a working `th` usually means a working bench.

`agentic` defaults to microVM isolation; `--isolation host` runs the
engine as a plain subprocess, which is what you want when the microVM
tooling isn't installed. `convo` spawns its own daemon unless you point
it at a running one with `--url` + `--token`.

## What gets measured

| Metric     | What                                               |
| ---------- | -------------------------------------------------- |
| Score      | Pass/fail on the canonical test suite              |
| Iterations | Agent-loop iterations spent on the task            |
| Cost       | `cost_usd` from the LLM gateway (6 decimal places) |
| Wall time  | End-to-end seconds                                 |
| Tool calls | Count by tool name (used for regression bisects)   |

Output is JSON-lines plus a printed summary. The CI workflow promotes the summary to `docs/bench-badge.json` and appends to `docs/bench-history.md` so the README badge stays current.

## The Line

The "Line" is the rolling per-task score in `docs/bench-history.md`. Every merged change to `main` re-runs the bench and writes a new line; PRs that move The Line in the wrong direction are visible in review.

## Pitfalls

- **Token estimation:** the runner estimates token usage when the gateway omits it from the response. Cost math depends on it. See pearl th-eff0d0 commit history.
- **Repo Dolt vs global:** the bench prefers the repo's local Dolt over the global registry. Don't write bench-only state into your global pearls.
- **CMake / `[METRICS]` capture:** C++ tasks need `-DEXERCISM_RUN_ALL_TESTS` and the work dir named after the task. The harness handles this; if you add a new task type, replicate that contract.

## Agentic conversation suite (`smooth-bench convo`)

Pearl th-f19853. The scored suites above ask "did the agent solve the task?".
This one asks "was Big Smooth any good to talk to?" — the failure mode behind
the ical incident, where three contradictory calendar answers arrived in one
conversation and no single turn was obviously wrong.

An LLM **driver** plays a user across several turns on ONE canonical-protocol
session (same `sessionId` throughout, so the agent's own memory is under test),
then an LLM **judge** grades the whole thread 1–5 on helpfulness, correctness,
tool use, and **consistency across turns**, plus a rubric PASS/FAIL.

```bash
# whole suite — spawns its own `th daemon` on :8791, tears it down after
cargo run -p smooai-smooth-bench -- convo

# one scenario, against a Big Smooth you already have running
cargo run -p smooai-smooth-bench -- convo --only rapid-correction \
  --url http://127.0.0.1:8788 --token "$SMOOTH_LOCAL_TOKEN"

# stochastic — take a rate, not an anecdote
cargo run -p smooai-smooth-bench -- convo --trials 3
```

Driver/judge credentials come from `SMOOAI_GATEWAY_KEY`, falling back to the
first OpenAI-compatible provider in `~/.smooth/providers.json` (the same store
the daemon reads). Scenarios live in `crates/smooth-bench/convo-scenarios.toml`;
transcripts land as JSON-lines in `~/.smooth/bench-runs/convo-*/`.

**Deliberately not in `cargo test`** — every scenario is several live LLM turns.
Only the pure parsing/rendering logic is unit-tested.

### `expect_fail` scenarios

`rapid-correction` fires a correction 1.5s into the first turn — the th-3a912a
interrupt gap — and is marked `expect_fail`. While the gap is open it records
`XFAIL` and the suite stays green; the day interrupts land it records `XPASS`
and exits non-zero, which is the signal to drop the flag and keep it as a
plain regression test.

## Comparing models (`--model`, repeatable)

Pearl th-898ec6. `--model` is repeatable on both `agentic` and `convo`.
Pass it more than once and the suite runs per model, then prints a
leaderboard and a scenario × model grid.

```bash
# is the premium model actually better on OUR harness?
cargo run -p smooai-smooth-bench -- convo --model deepseek-v4-flash --model gpt-5.5
cargo run -p smooai-smooth-bench -- agentic --model deepseek-v4-flash --model claude-opus-4-8
```

Each model gets its own scratch subtree, and for `convo` its own daemon
and workspace — otherwise model B would inherit model A's memories and
leftover files, which is not a comparison. `--url` is rejected with
several models: that daemon owns its own routing, so N models would
silently produce N identical rows.

**The grid is the point.** The leaderboard tells you which model to pick;
the grid tells you which of our scenarios is broken. A row every model
fails is not six bad models — it is our scenario, our tools, or our
prompt, and `universally_failed` calls those out explicitly as the
harness backlog. Rows marked `⊘` are `expect_fail` scenarios: already
documented, so they stay out of that callout.

> ⚠️ **The 2026-08-07 result previously recorded here was invalid** and has
> been removed. `--model` was silently ignored on the host spawn path
> (the daemon reads `SMOOTH_AGENT_MODEL`; the bench set only
> `SMOOAI_MODEL`), so both rows of that "deepseek-v4-flash beats gpt-5.5"
> comparison were the SAME model and the difference was run-to-run
> variance. Fixed, with a regression test — see _The model pin_ below.

## Cost

Cost is measured from the gateway, not from the protocol.

llm.smoo.ai returns a request's price only in the **`x-litellm-response-cost`
response header**; the JSON body carries token counts and no cost at all.
The engine parses the body, so `AgentEvent::Completed.cost_usd` is always
`0.0` — and that zero propagates cleanly through a pipe that is otherwise
correct (`runner.rs` → `TurnUsage` → `eventual_response.data.data.usage.costUsd`).
Pearl **th-11f9bb** fixes it at the source, which is what makes cost work
for `th code`'s status bar and the daemon too.

Until then the bench measures it itself (`spend.rs`):

1. `GET /key/info` before and after each model's run — the delta is what
   that key spent. (`/spend/logs` is admin-only.)
2. Minus the bench's **own** driver and judge calls, read off
   `x-litellm-response-cost` on our own responses. Left in, a cheap agent
   graded by an expensive judge reads as expensive, inverting the exact
   comparison the column exists for. In practice the harness has been
   ~40% of a short run's total.

Two traps, both of which produced confidently wrong numbers before being
handled:

- **LiteLLM posts spend asynchronously.** A response whose header already
  says `0.00117` does not appear in `/key/info` for another second or
  two, so sampling immediately after a run undercounts it — and
  undercounts the _slowest_ model worst. Both samples now poll until the
  figure settles.
- **The key is shared.** Background traffic from the smoo-hub daemon or a
  `th code` session lands in the delta, and on a short suite it is the
  same order as the signal. The bench samples that drift for 10s before
  the suite and renders anything below 2× it as **`<noise`** rather than
  a precise-looking number — a cost column that ranks a premium model
  below a budget one is worse than an empty one, because someone acts on
  it. `$/pass` is suppressed for those rows too, and an unresolvable cost
  never breaks a rank tie.

For exact figures, run against a dedicated gateway key.

## The model pin

`--model` reaches the daemon through **two** environment variables, and
only one of them is the one it reads:

| var                  | read by                                   |
| -------------------- | ----------------------------------------- |
| `SMOOAI_MODEL`       | the polyglot `operator serve` launcher    |
| `SMOOTH_AGENT_MODEL` | **the daemon's `resolve_gateway_config`** |

The microVM path always set both (with a comment saying why). The host
path — which `convo` uses — set only `SMOOAI_MODEL`, so every model in a
matrix silently ran the daemon's own default. `apply_engine_env` now sets
both and `host_spawn_pins_both_model_vars` guards it.

> 🚨 **Known blocker (pearl th-c127d1, P0):** with the pin working, every
> model _except_ the default returns an **empty reply** — the daemon
> boots, accepts the turn, and the assistant says nothing. Confirmed for
> `gpt-5.5` and `claude-sonnet-5`; `deepseek-v4-flash` is fine. It is not
> the gateway (a direct tools-bearing POST returns an identical shape for
> both). Until that is fixed, a cross-model comparison will show
> non-default models as `INCONCLUSIVE`, which is the honest answer rather
> than a fabricated pass.

## Asserting on the answer and on tool use

Pearl th-0e86ee. A deterministic assertion could only target a workspace
file, which cannot see the two failure modes the real transcripts are
full of: a confident answer with an empty tool transcript, and a refusal
of work that was always permitted. Neither leaves a trace on disk.

```toml
[scenario.check]
kind = "deterministic"
# "did it look before it answered?"
tools_used = ["list_files"]
tools_forbidden = ["bash"]

[[scenario.check.asserts]]
answer = true             # target the spoken reply, not a file
contains = "9417"

[[scenario.check.asserts]]
answer = true
not_contains = "I'm unable"
```

`answer = true` and `file = …` are mutually exclusive, and
`pointer`/`missing`/`unchanged` are meaningless on an answer assertion —
all three are rejected at parse time.

## Publishing model scores

Pearl th-adf614. The Line (`docs/bench-badge.json`) answers _"is the agent
getting better?"_ — one number, one model, over time. The model scoreboard
answers a different question: _"which model should we run?"_ They are
separate badges on purpose; folding a per-model comparison into The Line
would make a routing change look like a quality regression.

```bash
smooth-bench convo --model deepseek-v4-flash --model gpt-5.5 \
  --scoreboard board.json
scripts/the-line/render-model-scores.sh board.json
```

That writes three artefacts, all from one pre-rounded source so the badge,
the table and the JSON can never disagree:

| file                        | for                                          |
| --------------------------- | -------------------------------------------- |
| `docs/model-scores.json`    | machine-readable, the scoreboard verbatim    |
| `docs/model-badge.json`     | the README shields endpoint (best model + %) |
| `docs/Model-Leaderboard.md` | the human table                              |

Tests: `bash scripts/the-line/test-model-scores.sh`.

Two things the renderer deliberately refuses to do:

- **Publish an unmeasured cost as `$0`.** A zero means the spend had not
  posted or the sample missed it — not that the model was free. Those
  render as `—`.
- **Let a one-trial run read as a ranking.** Any `--trials 1` board
  carries a warning that a one-scenario gap is noise. Use `--trials 3`
  before acting on a close result.

## Two client surfaces (`--surface`)

Pearl th-b3fe81. `th code --headless` and the Big Smooth PWA both speak
the **same canonical WebSocket to the same daemon** (`smooth_code::client`
has been canonical since th-a14138), so "run the bench against both" is
not two harnesses. It is two client codepaths onto one engine.

```bash
cargo run -p smooai-smooth-bench -- agentic --surface daemon   # as the PWA drives it
cargo run -p smooai-smooth-bench -- agentic --surface thcode   # as `th code` drives it
```

`daemon` (default) calls `run_via_canonical` directly. `thcode` goes
through `smooth_code::headless::run_headless_capture` — smooth-code's
OWN client, not a re-implementation — so what the bench exercises is
what `th code` actually runs.

Because the engine is shared, a **difference** between the two surfaces
is attributable to `th code`'s own layer: the cast role it requests and
the working directory it pins. A regression that shows up on both is in
the agent; one that shows up only on `thcode` is in the coding harness.
The surface is recorded on the run and in every JSONL record so the two
sets stay tellable apart after the fact.

## Where the scenarios come from

The original scenarios were written from first principles. The corpus
added in pearl th-084acc was written from **transcripts** — ~1,300 kv
rows out of the smoo-hub daemon's `~/.smooth/operator.db`. Reading them
back, the dominant real-world failure is not coding. It is **not
looking**: `do you see the repo in ~/dev/smooai/smoo-hub` was asked ten
times in a row, escalating through _"cd to it then"_, _"Search ~/dev"_,
_"Pretty sure it does bruh"_. Every individual reply was polite and
plausible; the conversation was a disaster.

The axes that came out of that read, each with at least one scenario:

| Axis                            | The turn it came from                                                |
| ------------------------------- | -------------------------------------------------------------------- |
| workspace discovery             | "do you see the repo in ~/dev/smooai/smoo-hub" (×10)                 |
| deep / recursive search         | "Keep looking deeper it's there"                                     |
| find-and-follow a procedure doc | "find the claude skill … make your own skill based on it"            |
| multi-item follow-through       | "Are you stuck" / "You didn't finish what you were doing"            |
| backfill incomplete records     | "look for the ones … that do not have posters and fill them in"      |
| groundedness                    | "Are you sure, can you look it up"                                   |
| over-refusal                    | "why can't you do that" / "I give you explicit approval to run that" |
| standing-instruction decay      | "can you remember to always add a poster" → later, missing posters   |
| injection in quoted text        | the grandma-env-vars message, run live twice                         |
| ambiguous target                | "this is not a poster for The Bear"                                  |

When adding a scenario, name the transcript it came from in a comment.
A scenario nobody can trace back to a real failure is a scenario nobody
will trust when it goes red.

## Supplementary suites (opt-in via `--scenarios`)

Some suites deliberately break the default suite's "reproducible anywhere,
no real services" rule, so they ship as separate files loaded with
`--scenarios <path>` rather than in `agentic-scenarios.toml`:

| File                        | What it covers                                                  | Constraints                      |
| --------------------------- | --------------------------------------------------------------- | -------------------------------- |
| `frontend-scenarios.toml`   | current-stack React/Next code (`useReactTable` vs stale idioms) | judge/hybrid; pins a library set |
| `greenfield-scenarios.toml` | build-from-empty steering                                       | judge/hybrid; empty workspace    |
| `new-tools-scenarios.toml`  | the personal-assistant tools shipped this cycle                 | see below                        |

**`new-tools-scenarios.toml`** exercises `get_weather`, `get_location`, and
`present_plan`:

```bash
smooth-bench agentic \
  --scenarios crates/smooth-bench/new-tools-scenarios.toml \
  --model gpt-5.6-luna --model deepseek-v4-pro --model gemini-3.6-flash \
  --scoreboard board.json
```

- `weather-lookup` — needs network egress, so run with `--isolation host`
  (the default); a default-deny microVM makes it INCONCLUSIVE.
- `location-where-am-i` — **macOS only** (`get_location` is
  `#![cfg(target_os = "macos")]`); off a Mac the tool doesn't exist and the
  scenario FAILS. Ungranted Location Services still counts as a pass — the
  tool RAN (returned setup guidance), which is all `tools_used` asserts.
- `plan-present` — exercises the `present_plan` tool in Auto mode (portable).

The bench is single-turn and does not toggle Plan/Auto mode or read the
`present_plan` **directive**, so the full **Plan → present → accept → execute**
flow is out of its reach. That flow is covered instead by the daemon WS smoke
suite — see [[Daemon-WS-Smoke-Test]].

## Related

- [[Daemon-WS-Smoke-Test]]
- [[Architecture-Overview]]
- [[../bench-history]]
