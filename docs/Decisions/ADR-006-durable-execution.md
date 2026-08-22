---
status: Accepted
date: 2026-08
deciders: Brent
supersedes: None
superseded-by: None
tags: [decision, daemon, scheduling, durability]
---

# ADR-006: Durable execution — extend `SqliteScheduleStore`, don't adopt apalis

#decision

**Date**: 2026-08
**Status**: Accepted
**Pearl**: th-2bcd7f (this decision); th-3c09d6 (scheduler/wake-up loop), th-ccf3a3 (cron + messaging gateway), th-8ac0af (agent-facing scheduler tool)

## Context

Big Smooth is an **always-on** daemon. Always-on implies things happen without a
human at the keyboard: a morning brief, "watch this PR and tell me", a follow-up
the agent set for itself, a multi-hour operator run that must not evaporate when
the laptop sleeps. That is the shape of the question behind th-2bcd7f — do we
want "local Temporal"?

Temporal-the-server is off the table on its face. `smooth-daemon` is a single
binary with zero runtime dependencies (no Postgres, no broker, no sidecar); a
workflow server is the exact class of thing the daemon exists to not need. So
the real question narrows to: **do we adopt an embeddable durable-execution /
job-queue library — concretely `apalis` with its `apalis-sql` SQLite backend —
or keep growing what we already have?**

### What exists today

`crates/smooth-daemon/src/schedule.rs` (354 lines) + `scheduler.rs` (277 lines):

- `Schedule { id, prompt, kind, enabled, next_due, last_run }`, with
  `ScheduleKind` = `EveryNSeconds` | `DailyAt { hour, minute }` (UTC only).
- `SqliteScheduleStore` — one JSON row per schedule in a local rusqlite file.
  Reads load all rows and filter in memory, which is correct at the row counts
  involved (single-digit to dozens).
- `spawn_scheduler` — a 30s tick loop (`operator.rs:753`) that calls `tick()`;
  `tick()` fires each due schedule through a `TurnDriver` and advances it. A
  driver error leaves the schedule due, so the firing is retried next tick.
- `OperatorTurnDriver` — fires the prompt at the daemon's own operator as a
  loopback WS client speaking the canonical protocol. Proactivity is "just
  another client", with no operator-side special case.
- CLI surface: `smooth-daemon schedule add|list|remove`.

### What is actually missing

Enumerating the gaps mattered more than enumerating the libraries:

1. **One-shot schedules.** Both `ScheduleKind` variants recur. "Remind me at
   16:00" and the agent's self-set follow-ups (th-8ac0af) have no representation
   at all. This is the biggest functional hole.
2. **Cron expressions and local time.** `DailyAt` is UTC-only; a "morning brief"
   that drifts an hour twice a year is a bug a human notices immediately.
3. **Catch-up policy.** A daemon asleep for eight hours wakes with schedules
   past due and fires all of them at once. There is no coalesce/skip/backfill
   choice — it just stampedes.
4. **Retry budget.** A firing that fails stays due _forever_, retried every 30s
   with no backoff and no give-up. th-e979ac solved exactly this shape for Dolt
   (`retry_on_lock_flap`, `crates/smooth-pearls/src/dolt.rs:1214` — recover-once
   then exponential backoff + jitter to a bounded budget) and the scheduler
   never got the benefit.
5. **Result visibility.** `TurnDriver::drive` returns `Ok(())`. Nobody can ask
   "did the 06:00 brief run, and what did it say?"
6. **Mid-task durability.** `operator_storage.rs:12` is candid: the OLTP slices
   are durable, but **checkpoints and knowledge delegate to the engine's
   in-memory stores**. A restart mid-turn loses the checkpoint, so
   resume-from-checkpoint is a promise the daemon cannot currently keep.

Note the shape of that list: five scheduling gaps and one durability gap, and
the durability gap is in the checkpoint store, not in the scheduler.

## Decision

**Extend `SqliteScheduleStore` and the tick loop. Do not adopt apalis. Do not
build a bespoke durable step store.**

Separately and independently: close the mid-task durability gap by giving the
daemon a **durable `CheckpointStore`** (rusqlite, mirroring the engine's
synchronous `CheckpointStore` trait) in place of `MemoryCheckpointStore`.

Concretely, in priority order:

1. Add `ScheduleKind::Once { at }` and mark it `enabled = false` (or delete it)
   after firing — unblocks th-8ac0af's agent-facing scheduler tool.
2. Add `ScheduleKind::Cron { expr, tz }` via the `cron` crate (parse-only, ~1
   small dependency) and store an IANA timezone alongside it.
3. Add an explicit catch-up policy per schedule — `Skip` (fire once, advance to
   the next future slot) as the default, `Backfill` opt-in.
4. Add `failures: u32` + `last_error: Option<String>` to `Schedule`; on a driver
   error, back off (reuse the th-e979ac backoff+jitter shape) and disable the
   schedule after a bounded budget instead of retrying forever.
5. Record the firing outcome (`last_status`, a truncated response summary) so
   `schedule list` can answer "did it run".
6. Wire a rusqlite `CheckpointStore` so a turn interrupted by restart is
   resumable.

Every item above is a field on an existing struct or a branch in an existing
match. None of it is a dependency.

## Reasoning

### apalis is a job queue, not durable execution

This is the load-bearing point, and it is easy to miss because "durable
execution" and "durable job queue" sound alike.

Temporal's actual product is **deterministic replay**: workflow code is
re-executed from the start after a crash, with every side effect served from a
recorded history, so the program resumes mid-function with its local variables
intact. apalis does not do this and does not claim to. apalis gives at-least-once
job delivery, a retry/backoff layer, tower middleware, and (via `apalis-cron`)
cron-triggered job sources. Those are good things. They are not the thing the
pearl was worried about.

So "adopt apalis for durable multi-step workflows" would not deliver durable
multi-step workflows. It would deliver a queue, and we would still have to
decompose work into re-runnable steps ourselves — which is the entire cost of
the exercise, and is identical whether or not apalis is present.

### Replay is the wrong model for an agent turn anyway

Even granting a hypothetical Rust library that _did_ implement Temporal
semantics locally, it would not fit. Deterministic replay requires the workflow
body to be deterministic. An agent turn is an LLM sampling call whose output
chooses the next step; the tool calls it makes write files, push git refs, and
send mail. There is nothing to replay — re-running the "workflow" produces a
different workflow.

The primitive that fits a non-deterministic, side-effecting loop is
**checkpointing**: persist the conversation state after each step and resume
forward from the last one. The engine already models this (`CheckpointStore`,
a deliberately _synchronous_ trait — see the root `Cargo.toml` note choosing
sync `postgres` over async sqlx for exactly this reason). The daemon simply
wires the in-memory implementation. The fix is a ~150-line rusqlite impl of a
trait we already depend on, not a new execution model.

### The dependency cost is real and reverses two prior decisions

`apalis-sql` is `sqlx`-backed. Adding it pulls a **second SQLite driver** into a
binary that today links exactly one (`libsqlite3-sys` appears once in
`Cargo.lock`, via bundled rusqlite — the root `Cargo.toml` comment tracks this
deliberately), plus sqlx's macro/runtime surface and a tower middleware stack.

This repo has already made this call twice, on the record:

- Root `Cargo.toml`: rusqlite `bundled` is "the only crate in the tree that links
  it", with a note about the pain of unifying versions the last time two SQLite
  linkages coexisted (microsandbox's sea-orm → sqlx).
- SMOODEV-1468 chose the sync `postgres` crate over sqlx for the checkpoint
  store, specifically because the trait is synchronous.

Reversing both, for a library whose cron+retry surface is ~10% of what we would
pull in, is a bad trade at this size.

### The queue's value scales with workers, and we have one

Job queues earn their complexity through multi-consumer dispatch: visibility
timeouts so two workers don't double-run a job, leases and heartbeats, fair
scheduling, dead-letter queues. `smooth-daemon` is **single-tenant,
single-process, single-writer** — one daemon per human, firing on the order of
tens of jobs per day into an operator that runs them in-process. At N=1 worker,
every one of those mechanisms is machinery guarding against a race that cannot
occur.

### Option C (bespoke durable step store) is speculative

Writing our own workflow/step-state engine has the same "decompose into
re-runnable steps" cost as apalis, plus we own the bugs. And there is no
concrete workflow demanding it today: th-3c09d6's own note says the multi-step
routine primitive is the engine's `conversationWorkflow` (goal + steps,
judge-advanced) and explicitly warns _"do NOT reimplement multi-step routine
machinery"_. The scheduler's job is to be the **trigger**. A second workflow
engine underneath the one the engine already ships is the exact duplication that
note is guarding against.

### What would change this decision

Stated up front so the revisit is evidence-driven, not vibes:

- **More than one process consumes the same queue** — e.g. dispatch fans out to
  worker processes again. That is when leases and visibility timeouts stop being
  theater.
- **Non-agent jobs need at-least-once across process crashes** — a real outbox
  (webhooks, outbound mail retries) where a lost job is a lost customer message,
  not a missed brief.
- **Volume past ~1k rows/day**, where "load every row and filter in memory"
  stops being the right shape and we want indexed `next_due` queries with
  `LIMIT`. (Note: that is a 20-line change to `SqliteScheduleStore`, not
  necessarily a library.)

Any one of those is a reason to re-open this ADR. None of them is true today.

## Implementation

- `crates/smooth-daemon/src/schedule.rs` — `ScheduleKind::Once`/`Cron`, catch-up
  policy, failure/backoff fields, outcome fields. Schema stays "one JSON row per
  id", so existing rows deserialize as long as new fields carry
  `#[serde(default)]`.
- `crates/smooth-daemon/src/scheduler.rs` — `tick()` honors the catch-up policy,
  applies backoff, disables past the budget, and records the outcome;
  `TurnDriver::drive` returns a response summary instead of `()`.
- `crates/smooth-daemon/src/main.rs` — `schedule add --once/--cron/--tz`,
  `schedule list` shows last status.
- `crates/smooth-daemon/src/operator_storage.rs` — rusqlite `CheckpointStore`
  replacing `MemoryCheckpointStore` (independent of the scheduler work).
- New dependency: `cron` (expression parsing) and `chrono-tz`. That is the whole
  dependency budget for this decision.
- Migration: none. Existing schedule rows keep working; new fields default.

## Consequences

### Positive

- The zero-runtime-dependency, single-binary property is preserved. One SQLite
  linkage stays one SQLite linkage.
- Each gap is fixed where it actually is — one-shot schedules in the schedule
  type, mid-task durability in the checkpoint store — rather than by importing a
  framework that addresses neither directly.
- The scheduler stays ~600 readable lines with colocated tests, and stays
  "proactivity is just another WS client", which is what made it cheap to build.

### Negative

- We hand-roll cron semantics (catch-up, DST, backoff). These are genuinely
  fiddly, and the tests must carry the weight — DST boundaries and long-sleep
  catch-up are the adversarial cases.
- If a real multi-consumer durable-queue need arrives, we will do a migration
  we could have front-run. Accepted: the migration is bounded, and paying for it
  now on speculation is worse.
- No dead-letter queue, no job history table. Failures show as a disabled
  schedule with a `last_error`, which is thinner observability than a queue's.

### Neutral

- This ADR does **not** rule out apalis forever; it rules it out at current
  scale, with named triggers for reconsidering.
- Temporal-the-server remains permanently out of scope for the daemon,
  independent of any of the above.

## Related

- [[ADR-004-remove-microvm-sandbox-stack]] — the same instinct applied to
  runtime isolation: remove the heavy substrate, keep the property that mattered
- [[../Architecture/Architecture-Overview]]
- Pearls: th-2bcd7f (this decision), th-3c09d6 (scheduler/wake-up loop),
  th-ccf3a3 (cron + messaging gateway), th-8ac0af (agent-facing scheduler tool),
  th-e979ac (the backoff+jitter retry this generalizes)
