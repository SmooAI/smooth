---
'@smooai/smooth': patch
---

bench: cross-model leaderboard + an agentic corpus mined from real Big Smooth transcripts

`--model` is now repeatable on `smooth-bench agentic` and `smooth-bench convo`, so
one invocation scores several models and prints a leaderboard plus a scenario ×
model grid. Each model gets its own scratch subtree (and, for convo, its own
daemon and workspace) so model B never inherits model A's memories. The grid
surfaces scenarios that *every* model fails — those are harness bugs, not model
bugs, and they were invisible in any single-model run.

Fourteen new scenarios (nine agentic, five convo) written from ~1,300 rows of the
smoo-hub daemon's real conversation history rather than from first principles:
workspace discovery, recursive search, find-and-follow a procedure doc,
multi-item follow-through, backfilling incomplete records, groundedness,
over-refusal, standing-instruction decay, injection carried inside quoted
third-party text, and ambiguous targets.

Deterministic assertions can now target the agent's spoken answer (`answer = true`)
and the turn's tool calls (`tools_used` / `tools_forbidden`) — the two failure
modes that leave no trace on disk.
