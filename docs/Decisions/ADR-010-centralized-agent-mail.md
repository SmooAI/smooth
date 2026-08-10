---
status: Accepted
date: 2026-08-09
pearl: th-374f85
---

# ADR-010 — Agent mail moves from per-repo Dolt to a machine-level SQLite store

## Status

Accepted (2026-08-09, pearl th-374f85).

## Context

`th agent` (the roster) and `th msg` (the mailbox) shipped in pearl th-70aaef on
top of the per-repo Dolt pearl store: two extra tables (`agents`, `messages`) in
`.smooth/dolt/`, syncing to teammates over `refs/dolt/data` like pearls do.
Reusing the store we already had was the right call to get the feature out. In
daily use with many parallel agents it produced three problems, and all three
are structural rather than bugs.

**1. The mailbox was a function of where you were standing.** Store resolution
walked up from the cwd (`find_dolt_dir()`), so an agent in
`~/dev/smooai/smooth-th-abc123/` and an agent in `~/dev/smooai/smooth/` had two
*different, unconnected* mailboxes for the same repo — and an agent in a
different repo entirely had a third. Since every unit of work happens in its own
worktree, the common case was agents that could not reach each other, silently.
An agent is a property of the **machine**, not of a checkout.

**2. Dolt is single-writer, and mail is the most concurrent thing we do.** A
handful of agents sending and polling at once wedged the *entire* store with
`Error 1105: cannot update manifest: database is read only` — which blocks pearl
writes too, so a mail storm took work tracking down with it. We papered over it
twice (retry-with-backoff in th-e979ac, then telling the watcher not to `--pull`
in the th-mail skill) without fixing the cause.

**3. Every send paid for durability it did not want.** A `th msg send` cold-booted
Dolt (~0.7s), wrote, committed, and pushed to a git remote — for a message whose
useful lifetime is minutes. The SessionStart hook had to background its
registration just to keep session start responsive.

Underneath all three: **mail is ephemeral local coordination, and we stored it in
a version-controlled, team-synced, single-writer database.** Pearls genuinely
want that. Mail never did.

## Decision

Agent mail and the agent roster move to **one SQLite database per machine** at
`~/.smooth/mail.db` (`$SMOOTH_MAIL_DB` overrides, which is how the tests get
isolation). WAL mode, a 10s busy timeout, `rusqlite` with the bundled engine —
no new external dependency; the daemon already links it.

- **`smooth_pearls::MailStore`** is the whole API. Concrete struct, not a trait:
  a trait with one implementation buys nothing today, and the method signatures
  are shaped so one can be extracted when a second backend actually exists.
- **Read state is per-recipient** (`message_reads(message_id, agent, read_at)`),
  replacing the single `read_at` column. This fixes broadcast first-reader-wins,
  where whichever agent looked at a `to = all` message first marked it read for
  everyone.
- **Messages are typed and prioritized** (`note|request|result|handoff|cancel`,
  integer priority) so a recipient can triage without reading, and urgent mail
  sorts to the top of the inbox.
- **Agents publish presence and context**: `idle|working|waiting|offline`, a
  free-form `task`, and the repo/worktree/branch they registered from — so
  `th agent list` answers "who is around and what are they doing".
- **Dead agents are reaped**: `list` flips any row whose recorded pid is no
  longer alive to `offline`. The pid must be supplied (`--pid $PPID` from the
  SessionStart hook, or `$SMOOTH_AGENT_PID`) and is deliberately *never* `th`'s
  own — `th` is a one-shot child of the real session, so recording its pid would
  mark every agent offline a second after registering.
- **The Dolt-era sync flags become no-ops.** `--no-push`, `--pull`, `--no-pull`
  still parse (the SessionStart hook and existing scripts pass them) and print a
  deprecation note to stderr. There is no remote to sync with.

`Mailbox` and `AgentRegistry` (the Dolt implementations) are left in place and
untouched, so old per-repo data stays readable. **Nothing is migrated** — mail
is ephemeral coordination chatter, and the fragmented pre-migration mailboxes
are exactly the state this ADR exists to stop reproducing.

## Consequences

**Good.** One mailbox per machine, so agents find each other regardless of
worktree. Sends are sub-millisecond local writes. Concurrent writers queue on
SQLite's lock instead of wedging a shared store — the covering test fires 80
sends from 8 threads and expects all 80 to land. Broadcast read state is
correct. `th msg watch --once` becomes a real primitive, so the th-mail skill's
watcher script drops from a hand-rolled poll loop to a wrapper.

**Bad / accepted.** Mail no longer crosses machines. That was a real capability
of the Dolt design, and in practice it was never used — agents that coordinate
run on the same host, and the cross-machine path was the one that caused the
lock storms. The follow-up below is where it comes back, if it is ever wanted.

**Also accepted.** Old per-repo mailboxes are stranded (readable via the old
API, invisible to the CLI). Reaping needs a caller-supplied pid, so agents
registered without one are judged only by `last_seen`.

## Alternatives considered

**Keep Dolt, use one global store (`~/.smooth/dolt`).** Fixes fragmentation
only. The single-writer wedge and the ~0.7s boot per send remain, and those are
what actually hurt.

**Postgres / a local server process.** Correct concurrency, wrong shape: `th` is
a zero-runtime-dependency single binary, and a mailbox that needs a server
running is a mailbox that is down when you need it.

**A file-per-message maildir.** No dependency at all, but we would hand-roll
indexing, atomic read-state, and ordering — more code than the SQLite schema,
for less.

## Follow-up (not in this change)

A **cloud backend on api.smoo.ai** as an **opt-in** second `MailStore` backend:
user-scoped, so an operator's agents can reach each other across machines. It is
explicitly opt-in and Smoo hosting is **never required** — local SQLite stays
the default and works fully offline, forever. There will be **no Dolt mail
backend**; that door is closed.

## Related

- Pearl th-374f85 (this change), th-70aaef (the original Dolt mailbox)
- [[ADR-Index]]
- [[../Engineering/Using-th-CLI]]
