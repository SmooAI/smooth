# Pearls

#architecture

> [!info] Work items, one database
> A pearl is a unit of work. Every project's pearls live in **one SQLite file per machine**, `~/.smooth/pearls.db`, keyed by the project's canonical root (pearl th-d3e842). Reads are ~10ms, concurrent agents queue on SQLite's lock instead of wedging, and a pearl created inside a git worktree lands in the main checkout's project.

## Concepts

- **Pearl** — title, description, status, priority, type, dependencies, labels, comments, history.
- **Status** — `open`, `in_progress`, `closed`, `deferred`.
- **Type** — `task`, `bug`, `feature`, `epic`, `chore`.
- **Dependencies** — DAG. `ready` pearls have no open blocking dependencies.
- **Sub-pearls** — pearls created by operators mid-dispatch (`delegate` tool). Linked via parent.
- **Memories** — free-form project notes (`th pearls remember`), same database, same project scoping.

## Storage layout

```
~/.smooth/pearls.db          # SQLite, WAL mode; $SMOOTH_PEARLS_DB overrides
  pearls              (project, id) PRIMARY KEY
  pearl_dependencies  (project, pearl_id, depends_on)
  pearl_labels        (project, pearl_id, label)
  pearl_comments      (project, id) UNIQUE, seq for ordering
  pearl_history       (project, id) UNIQUE, seq for ordering
  memories            (project, id) UNIQUE
  config              (project, k)
~/.smooth/registry.json      # git-repo project roots th has opened (+ anything `th pearls init`-ed); dead + non-git paths pruned on open
```

**`project`** is the canonical project root. `PearlStore::open(any_path)` runs
`git rev-parse --git-common-dir` and takes its parent, so the main checkout and
every linked worktree of a repo share one project; outside git it is the
directory itself. Ids stay `th-xxxxxx` and are unique **per project** — two
projects may reuse an id (the Dolt-era stores generated them independently).

Timestamps are UTC RFC3339 text with a fixed microsecond width, so `<=` in SQL
is chronological. Queries compare against a Rust `Utc::now()` literal, never
SQLite's `now`.

## `th pearls` quick reference

```bash
th pearls init                        # ensure the db exists + register this project
th pearls create --title="…" --description="…"
th pearls list --status=open
th pearls list --status=in_progress
th pearls show <id>                   # details + deps + comments + history
th pearls checkpoint <id> --note "…" --next "…"   # handoff checkpoint (see below)
th pearls show <id> --handoff [--json]   # handoff packet
th pearls prime --in-progress [--cwd .] [--assignee a] [--json]
th pearls update <id> --status=in_progress
th pearls close <id1> <id2> …
th pearls ready                       # open, no blockers
th pearls blocked                     # open, unmet deps
th pearls projects                    # all registered projects
th pearls push / pull                 # exit-0 notice: sync is pearl th-19cca5
th db path                            # where pearls.db lives
```

There is no `th issues` or `th beads` alias. The naming lineage is beads → issues → **pearls**; only "pearls" is current.

## History

Until pearl th-d3e842 the store was an embedded Dolt database per project
(`.smooth/dolt/`, a 140MB `smooth-dolt` Go binary, sync over `refs/dolt/data`).
PR #522 retired it for the SQLite store above and shipped a one-shot
`th pearls migrate-from-dolt` importer; pearl th-c6ba83 then deleted the Dolt
shim, the Go binary and the Go/ICU CI steps. A straggler machine that still has
a `.smooth/dolt/` store must install **th ≤ 0.42.x** (Homebrew v0.42.1 is the
last release with `migrate-from-dolt`), run `th pearls migrate-from-dolt` in
each repo, and only then upgrade — newer builds cannot read Dolt at all.

## Handoff model — checkpoints (pearl th-9483e8)

A pearl is the unit of work that outlives a context window, so it carries the
state a fresh session needs to resume cold. `th pearls checkpoint <id>` appends
a **checkpoint**: an optional note, a timestamp, an `auto` flag, and a
**handoff** block collected from git at that moment — `worktree`, `branch`,
`head`, `dirty` (`git status --porcelain`, capped at 50 rows),
`agent_session_id` (`--session-id` or `$CLAUDE_SESSION_ID`), and `next`
(`--next`, what to do first).

**Storage.** Each checkpoint is one ordinary pearl comment whose content is
`smooth-checkpoint:` followed by the JSON record. That rides on the public
`PearlStore` comment API alone — no table, no migration. `th pearls show` folds these
comments into a `Checkpoints` section instead of printing the JSON. The
effective handoff is the field-wise merge of every checkpoint in order (latest
non-null wins), so an `--auto` checkpoint refreshes `head`/`dirty` without
erasing an earlier `next`; notes accumulate.

**Packet.** `th pearls show <id> --handoff --json` and
`th pearls prime --in-progress --json` emit the SmoothFlow "pearl rail" shape:

```json
{
    "pearl": { "id": "th-9483e8", "title": "…", "status": "in_progress", "…": "…" },
    "handoff": {
        "worktree": "/…/smooth-th-9483e8-handoff",
        "branch": "th-9483e8-handoff",
        "head": "…",
        "dirty": ["M …"],
        "agent_session_id": "…",
        "next": "…"
    },
    "checkpoints": [{ "at": "2026-09-07T22:10:00Z", "note": "…", "auto": false }],
    "blocks": ["th-d3e842"],
    "pr": { "number": 520, "url": "…", "state": "OPEN", "ci": "pending" }
}
```

`blocks` is the open pearls this one still waits on; `pr` comes from
`gh pr list --head <branch>` (null without `gh`, offline, or no PR), with `ci`
folded from `statusCheckRollup` to `success` / `failure` / `pending`. The
human form is a compact "resume cold" block: what it is, where it is, what
happened, what is next.

**Worktree matching.** `--cwd <dir>` keeps the pearls whose recorded worktree
is `<dir>`'s repo root, or whose id appears in `<dir>`'s branch name — the
`th worktree create th-<id>-…` convention — so a claimed-but-never-checkpointed
pearl is still found. `PearlStore::open` resolves the project through the git
common dir, so a checkpoint taken inside a linked worktree lands under the
project's root, not the worktree's.

**Hooks (smooth-agent plugin).** `PreCompact` runs
`th pearls checkpoint --auto` for the matching in-progress pearls;
`SessionStart` with matcher `compact|resume` injects
`th pearls prime --in-progress --cwd .` as `additionalContext`. Both are
silent no-ops without `th`, a store, or a match.

## Context budget and `/th-clear` (SMOODEV-3759)

`/th-clear` is a handoff that continues in the **same terminal** with a fresh
context. A plugin can't clear its own harness, so it arms a one-shot resume
that the user's `/clear` triggers:

1. The `th-clear` skill does the th-handoff checkpoint, then
   `th harness handoff arm --pearl <id> --harness claude-code`. That writes a
   token to `~/.smooth/handoff/armed/<cwd-hash>-<harness-pid>.json`
   (`$SMOOTH_HOME` respected). The harness PID (`CLAUDE_PID`, else a
   process-tree walk to the `claude` process) stays the same across `/clear`
   and tells two terminals in one checkout apart. It prints `/clear` and
   `/th-handoff resume <pearl>` and copies the resume line to the clipboard
   (`--no-copy` skips that), so a missed claim is one paste away.
2. The user types `/clear`. The `SessionStart` hook (matcher `clear|startup`)
   runs `th harness handoff claim`. It takes the token by atomic rename (one
   claimer wins), re-checkpoints each pearl with the **new** session id (so the
   compact/resume hooks keep matching it in a primary checkout) and injects the
   rendered handoff. With no token it prints nothing: a plain `/clear` is
   untouched. `compact`/`resume` never claim.
   **Expiry (SMOODEV-3888).** The arming terminal (an owner-PID match) claims
   for 24 hours: the PID is what makes the match safe, so a `/clear` typed an
   hour later still resumes. Before this, a 15-minute TTL reaped the token
   silently. `--ttl-minutes` (default 60) now bounds only the fallbacks that
   can't prove the terminal, such as a fresh launch.
   **No silent misses.** If a token for this directory exists but this session
   may not take it (expired, or another terminal's), the hook injects a short
   note naming the pearl and `th pearls show <id> --handoff` instead of
   nothing. An expired token whose terminal has exited is reaped once it has
   been reported.
3. The first message after `/clear` (e.g. "go") starts the fresh session on it.

A fresh launch (`startup`) in the same directory claims too, but only when
exactly one unexpired token is armed there. `th harness handoff list` / `disarm` inspect
and drop tokens.

**Budget.** `th harness budget` runs on `UserPromptSubmit` and `PostToolUse`.
It reads the context size and model from the last main-thread assistant `usage`
in `transcript_path` (input + cache-creation + cache-read tokens) and only ever
adds context: **nothing blocks**, so when to start fresh stays the user's call.
State lives in `~/.smooth/handoff/budget/<session>.json`; a context under half
the flagged peak means the session compacted, which re-arms every note.

- **Suggest (150k).** Past `harness.context_suggest`, finished work — `gh pr
  merge`, `th worktree merge` or `th pearls close` — asks the agent, once per
  context segment, to mention `/th-clear` in its final report if the user's
  task is done, and otherwise to keep going. Commits, pushes, PRs and green
  tests are mid-task, and a suggestion there stalled sessions (SMOODEV-4020):
  auto-compaction is what keeps a long session going.
- **Warn (270k).** Once: auto-compaction is near, checkpoint the pearl so the
  re-injected handoff is current.
- **Reset (300k).** The harness auto-compacts there. A context past budget +
  10% (a session launched before `th harness enable`) gets one note to
  checkpoint the pearl and keep working; its native compaction still comes.

**Statusline.** The smooth-agent statusline (`th doctor --setup-statusline`)
shows the context against the session's budget, `◔ 162k/300k 54%`, from
`th harness budget --statusline` (Claude Code's `context_window.current_usage`,
else the transcript). It is dim under the suggest line, yellow past it (a good
time to `/th-clear`) and red past the warn line. Codex and OpenCode have no
scriptable status line, so they get the hook notes only.

**Auto-compaction is the default reset (SMOODEV-3891).** Every call resends the
whole context, so long sessions on 1M-context models dominate input spend, and
native auto-compaction only fires near the window (~970k). Compaction itself is
cheap (it cut 80–90% of the context for ~0.3% of what it saved, measured across
a month of transcripts), so `th harness enable` makes it fire at the budget:
Claude Code gets `autoCompactWindow` in `~/.claude/settings.json` (clamped to
100k–1M), Codex gets `model_auto_compact_token_limit` in `~/.codex/config.toml`.
A value th did not write is the user's and is left alone; what th wrote is
recorded in `~/.smooth/harness/compact-owned.json` so `disable` and a budget of
0 remove only that. The harness reads the threshold at launch, so new sessions
pick it up. The pearl handoff rides along: `PreCompact` checkpoints, and the
`SessionStart` `compact` hook re-injects it on top of the harness's summary.
`/th-clear` stays the manual fresh start.

| Setting                       | Default   | Effect                                                                                                                                                                                          |
| ----------------------------- | --------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `harness.context_suggest`     | 150000    | past it, a merge or pearl close has the agent mention `/th-clear`, once per segment                                                                                                            |
| `harness.context_suggests`    | (none)    | per-harness / per-model suggest overrides, same `selector=tokens` form: `*/claude-sonnet-*=120000`                                                                                              |
| `harness.context_budget`      | 300000    | the reset point. `compact`: the harness auto-compacts there; a context past budget + 10% gets a note to checkpoint. `clear`: one note to run th-clear                                         |
| `harness.context_budget_warn` | 270000    | one note. `compact`: checkpoint the pearl at the next clean stopping point. `clear`: wrap up and run th-clear                                                                                   |
| `harness.context_budgets`     | (none)    | overrides as `selector=tokens`: `codex=200000`, `claude-code/claude-opus-*=300000`, `*/claude-sonnet-*=180000`. Most specific wins (harness+model > model > harness); warn keeps the global gap |
| `harness.context_reset`       | `compact` | `compact` (native auto-compaction) or `clear` (the `/th-clear` flow; `th harness enable` then removes th's threshold)                                                                           |

`0` disables a budget. Set with `th settings set harness.context_budget <n>`,
then `th harness enable claude-code` (and `codex`) to sync the native
thresholds; `th harness status` shows them. Per-model overrides apply to the
hook only: a harness has one compaction threshold, set from its harness-level
budget.

**Measure it.** `th harness budget --report [--days 30] [--json]` reads
`~/.claude/projects/*/*.jsonl` (main thread only, deduped by message id) and
shows input tokens by context size at the call, the auto and manual
compactions (median context before → after), the share of input carried by
calls already over their budget, and upper-bound estimates of what resetting
at the budget, or taking every `/th-clear` suggestion, would have saved (each
reset to ~30k).

## Diver: the lifecycle wrapper

The pearl store is a passive CRUD surface. The [[The-Cast#Diver|Diver]] cast member wraps it with lifecycle semantics:

- `Diver::dispatch(title, desc, parent?)` — create pearl, mark `in_progress`, return id.
- `Diver::complete(id)` — close pearl after successful dispatch.
- `Diver::sub_pearl(parent, …)` — create a child pearl during a dispatch.
- Jira sync (bidirectional) when `JIRA_URL` + `JIRA_API_TOKEN` are configured.

## Related

- [[Data-Storage]]
- [[The-Cast]]
