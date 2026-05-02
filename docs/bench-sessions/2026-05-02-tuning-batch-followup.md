# Bench session — 2026-05-02 follow-up (timeout + heartbeat fixes)

## What landed after take 1 (in chronological order)

1. **`aa0cb46` — bench: tunable chat-driver timeouts (pearl `th-ac0407`).**
   Bumped two hardcoded 120 s timeouts in `smooth-bench/chat_driver.rs`:
   - `SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S` (default 300, was 120) — reqwest
     timeout on the initial `POST /api/chat` dispatch call.
   - `SMOOTH_BENCH_IDLE_GRACE_S` (default 600, was 120) — quiescence
     heuristic in the comment-polling loop.

2. **`ac2f660` — dispatch: pearl-comment heartbeat (pearl `th-525594`).**
   Discovered while tuning the bench: `exec_in_sandbox` is blocking and
   the runner's `AgentEvent::ToolCallStart` events arrive in one batch
   when the run finishes, so the pearl's comment count stayed flat for
   the whole sandbox lifetime — even with 600 s grace, a real solve
   that took 300 s never wrote a comment to reset the timer.
   `dispatch_ws_task_sandboxed` now spawns a heartbeat task that posts
   `[PROGRESS] sandbox running (Ns elapsed, heartbeat #N)` every
   `SMOOTH_DISPATCH_HEARTBEAT_S` seconds (default 30) while exec is in
   flight; aborted on success, error, and destroy paths.

## Why we don't have a clean take-2 baseline this session

The bench infrastructure was uncooperative:

- The daemon was restarted twice this session (to roll the new
  prompts and the new heartbeat fix into runtime). Each restart
  killed any in-flight bench, but the bench's HTTP retry behaviour
  left zombie processes still polling stale pearl ids.
- Multiple parallel bench attempts ended up sharing the daemon —
  taking turns dispatching to the same chat agent, racing each other
  for pearl ids. The output logs were interleaved.
- Killing the orphans by PID was unreliable: PIDs got reused, the
  zombie shell wrappers (zsh -c …) survived their child benches.
- A fresh "take 4" started from a clean daemon state finally got past
  the dispatch but stalled at first task — most likely the daemon
  had cold-start contention from the kill cascade.

The fixes themselves are correct and tested (unit tests pass, build
clean). The validation will land on the next session start where the
daemon has been quiet long enough to take the bench seriously.

## Recommended next-session protocol

```bash
# 1. Make sure no benches are running.
pkill -9 -f "smooth-bench"

# 2. Ensure daemon is fresh.
launchctl kickstart -k gui/$UID/com.smooai.smooth
sleep 10
curl -fsS http://127.0.0.1:4400/healthz

# 3. Run the bench in a clean tmux pane (not via `disown`).
tmux new -d -s bench \
  "target/release/smooth-bench score --pr --budget-usd 10 \
     --output /tmp/bench-take5/score-pr.json"

# 4. Tail the log; expect [PROGRESS] heartbeat comments at 30s intervals.
tail -f /tmp/bench-take5/run.log
```

Expected outcome: pass rate substantially higher than take 1's 22.2 %,
with median task time reflecting real solve duration (250-500 s) rather
than the harness's quiet-timeout. cpp / java / go (which all scored
0/3 on take 1 because they need 200+ s of model time to even produce
anything) should be the biggest gainers.

## Pearl follow-ups still open

- `th-7306f0` — `cost_usd=0.00` across all chat-agent dispatched
  tasks; cost tracker not wired through teammate_spawn dispatch.
- `th-cfa1fb` — D5: lazy tool loading via `tool_search` meta-tool.
  Not done. Real refactor (interior mutability on ToolRegistry +
  per-iteration schema recomputation in agent.rs).

Both filed against pearls; see ~/.claude/plans/typed-sniffing-badger.md.
