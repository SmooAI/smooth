# Bench Harness

#engineering

> [!info] How we measure ourselves
> `th bench` runs Exercism-style problems through the agent loop with deterministic scoring. The dashboard's "The Line" tracks the rolling score over time so we can tell when a change made the agent better or worse.

## The crate

`crates/smooth-bench/` owns the harness. Curated tasks live in `crates/smooth-bench/curated-tasks.toml`. Each task is a directory of problem statement + tests + reference solution; the harness scores by running the test suite.

## Running locally

```bash
th bench                             # Run the curated suite
th bench --task <id>                 # Run a single task
th bench --print                     # Pretty-print results + cost
```

The harness talks to the same host-process daemon everything else does (`th up`; the microVM mode is gone — [[../Decisions/ADR-004-remove-microvm-sandbox-stack]]). It needs the native `smooth-operative` binary in `target/release/` — build it with `cargo build -p smooth-operative --release` before running.

It also sets `SMOOTH_WORKFLOW_SKIP_TEST=1` so the TEST phase doesn't add tests of its own (which would skew the score). The harness runs the canonical test suite itself, post-agent.

## What gets measured

| Metric          | What                                                            |
| --------------- | --------------------------------------------------------------- |
| Score           | Pass/fail on the canonical test suite                           |
| Iterations      | Agent-loop iterations spent on the task                          |
| Cost            | `cost_usd` from the LLM gateway (6 decimal places)               |
| Wall time       | End-to-end seconds                                              |
| Tool calls      | Count by tool name (used for regression bisects)                 |

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
> variance. Fixed, with a regression test — see *The model pin* below.

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
  undercounts the *slowest* model worst. Both samples now poll until the
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

| var | read by |
|---|---|
| `SMOOAI_MODEL` | the polyglot `operator serve` launcher |
| `SMOOTH_AGENT_MODEL` | **the daemon's `resolve_gateway_config`** |

The microVM path always set both (with a comment saying why). The host
path — which `convo` uses — set only `SMOOAI_MODEL`, so every model in a
matrix silently ran the daemon's own default. `apply_engine_env` now sets
both and `host_spawn_pins_both_model_vars` guards it.

> 🚨 **Known blocker (pearl th-c127d1, P0):** with the pin working, every
> model *except* the default returns an **empty reply** — the daemon
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
times in a row, escalating through *"cd to it then"*, *"Search ~/dev"*,
*"Pretty sure it does bruh"*. Every individual reply was polite and
plausible; the conversation was a disaster.

The axes that came out of that read, each with at least one scenario:

| Axis | The turn it came from |
|---|---|
| workspace discovery | "do you see the repo in ~/dev/smooai/smoo-hub" (×10) |
| deep / recursive search | "Keep looking deeper it's there" |
| find-and-follow a procedure doc | "find the claude skill … make your own skill based on it" |
| multi-item follow-through | "Are you stuck" / "You didn't finish what you were doing" |
| backfill incomplete records | "look for the ones … that do not have posters and fill them in" |
| groundedness | "Are you sure, can you look it up" |
| over-refusal | "why can't you do that" / "I give you explicit approval to run that" |
| standing-instruction decay | "can you remember to always add a poster" → later, missing posters |
| injection in quoted text | the grandma-env-vars message, run live twice |
| ambiguous target | "this is not a poster for The Bear" |

When adding a scenario, name the transcript it came from in a comment.
A scenario nobody can trace back to a real failure is a scenario nobody
will trust when it goes red.

## Related

- [[Architecture-Overview]]
- [[../bench-history]]
