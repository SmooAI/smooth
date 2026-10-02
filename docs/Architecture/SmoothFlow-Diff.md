# SmoothFlow Diff — structured diffs by turn, and the review loop

> Epic th-26f5b9 · phase 1: the engine's structured diff service + the Mac
> viewer. Phase 2 is the Linux/Windows desktop app (GPUI) and the phones.
> Client behavior is normative in
> [SmoothFlow-Client-Spec.md §14](SmoothFlow-Client-Spec.md#14-diff); this is
> the engine side and the wire.

The Diff tab used to be `git diff --stat -p` in a text view. It is now a
review tool: a file tree with viewed marks, unified or side-by-side, syntax
and word-level highlight, noise collapsed, per-hunk revert and stage, and
comments that go back to the agent as one steer. Its own angle is **diffing by
agent turn**: "what did the last turn change", separate from your own edits.

**Decision (2026-10-02):** each client renders natively; there is no shared
web view. The engine computes the diff — hunks, word spans, syntax token
spans — and clients only lay it out and paint it.

## Where it lives

| Piece                                          | Path                                                                                            |
| ---------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Wire model (serde types)                       | `crates/smooth-flow/src/diff/model.rs`                                                          |
| Pipeline, bases, hunk actions, review message  | `crates/smooth-flow/src/diff/mod.rs`                                                            |
| `git diff` parser + one-hunk patch writer      | `crates/smooth-flow/src/diff/parse.rs`                                                          |
| Snapshots, tree diffs, `git apply` (git glue)  | `crates/smooth-flow/src/diff/git.rs`                                                            |
| Turn-snapshot policy                           | `crates/smooth-flow/src/diff/snapshot.rs`                                                       |
| Word-level spans                               | `crates/smooth-flow/src/diff/words.rs`                                                          |
| Syntax spans (syntect + two-face)              | `crates/smooth-flow/src/diff/syntax.rs`                                                         |
| Noise classification                           | `crates/smooth-flow/src/diff/noise.rs`                                                          |
| Engine methods (`diff`, `diff_action`, …)      | `crates/smooth-flow/src/engine.rs`                                                              |
| WS dispatch + HTTP twins                       | `crates/smooth-daemon/src/flow_route.rs`                                                        |
| Shared client rules + `spec/vectors/diff.json` | `crates/smooth-flow-client/src/diff.rs`                                                         |
| Mac viewer                                     | `apps/smoothflow/Sources/UI/DiffView.swift`, `DiffLayout.swift`, `Sources/Flow/DiffModel.swift` |

## Bases

Every base is a tree-to-tree `git diff --find-renames -U3`; "the worktree
now" is itself a snapshot tree (below), so untracked files need no special
case and ignored files never appear.

| Base          | Left side                                                                                                                                 | Right side                                                                  |
| ------------- | ----------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `turn`        | the last turn's **start** snapshot                                                                                                        | its **end** snapshot, or the worktree now while the turn runs (`turn.live`) |
| `uncommitted` | `HEAD` (the empty tree in a repo with no commits)                                                                                         | the worktree now                                                            |
| `branch`      | merge base of HEAD with `origin/HEAD`, `origin/main`, `origin/master`, `main` or `master` (first that resolves; else HEAD, with a `note`) | the worktree now                                                            |

**Why start → end for an idle agent, not last end → now.** An idle agent's
turn is finished; diffing to "now" would pin your own edits since then on the
agent. While a turn runs there is no end yet, so the right side is the
worktree as it is. Without a start snapshot (a harness that only reports turn
ends), the previous end stands in for it. With no snapshot at all the diff is
empty and its `note` says why.

## Turn snapshots

- **When.** On a state transition (hooks, a native harness's events, or the
  pane scraper — all land in `Engine::set_state`): into `working` with no open
  turn → a `start` snapshot; into `idle`, `limited`, `done` or `dead` with an
  open turn → an `end` snapshot. `needs_you` is part of the turn. Shells take
  none. `SMOOTH_FLOW_TURN_SNAPSHOTS=0` turns them off.
- **How.** The user's index is **copied** to a temp file; `GIT_INDEX_FILE`
  points `git add --all` and `git write-tree` at the copy. The real index, the
  stash, refs and worktree are never written (a test asserts the index is
  byte-identical). Untracked files over 20 MiB are excluded from the snapshot.
- **Synchronous** inside the hook call that reports the transition, so the
  start tree is written before the agent's first edit of the turn.
- **Cost.** Time: one `add --all` with a warm stat cache plus `write-tree` —
  tens of ms on a small repo, a few hundred on a large monorepo. Storage: git
  objects only for content not already in the object store (changed files'
  blobs, changed directories' trees) plus one `flow.db` row
  (`diff_snapshots`: session, seq, kind, tree, at). The last 40 rows (20 turns)
  per session are kept; removing a session drops its rows. The objects are
  unreferenced, so `git gc` prunes them after `gc.pruneExpire` (two weeks); a
  diff whose snapshot was pruned fails with a message that says so.

## The model

`flow.diff`'s `diff` (and `GET …/diff`) is `diff::model::Diff`:

```jsonc
{
    "base": "turn",
    "from": { "ref": "<tree>", "label": "turn start" },
    "to": { "ref": "<tree>", "label": "turn end" },
    "turn": { "seq": 4, "started_at": "…", "ended_at": "…" }, // `live: true` while running
    "note": "…", // why it is empty or partial
    "files": [
        {
            "path": "src/lib.rs",
            "old_path": "…", // old_path: renames/copies
            "status": "added|deleted|modified|renamed|copied|mode_changed",
            "old_mode": "100644",
            "new_mode": "100755",
            "binary": true,
            "language": "Rust",
            "added": 3,
            "deleted": 1,
            "noise": "lockfile|generated|vendored|minified|large",
            "collapsed_by_default": true,
            "hunks_omitted": "collapsed|budget", // hunks not in this frame
            "truncated": true,
            "hunks": [
                {
                    "id": "9f2c…",
                    "old_start": 1,
                    "old_lines": 3,
                    "new_start": 1,
                    "new_lines": 4,
                    "section": "fn main() {",
                    "truncated": true,
                    "staged": true,
                    "lines": [
                        {
                            "kind": "add|del|ctx",
                            "old": 2,
                            "new": 2,
                            "text": "…",
                            "no_eol": true,
                            "cr": true,
                            "truncated": true,
                            "syntax": [[0, 3, 0]], // [start, end, kind] — kind indexes `legend`
                            "words": [[4, 9]],
                        },
                    ], // changed span vs the paired line
                },
            ],
        },
    ],
    "added": 3,
    "deleted": 1,
    "truncated": true,
    "files_omitted": 12,
    "legend": [
        "keyword",
        "string",
        "comment",
        "number",
        "constant",
        "function",
        "type",
        "variable",
        "property",
        "operator",
        "punctuation",
        "tag",
        "attribute",
        "macro",
        "escape",
        "heading",
        "link",
    ],
}
```

- Offsets in `syntax` and `words` are **Unicode scalar** offsets, half-open.
  Empty arrays and `false` flags are omitted from the wire.
- **Hunk ids** hash the path and the lines, not the line numbers, so reverting
  one hunk leaves the ids of the hunks below it unchanged. Two identical hunks
  in one file get distinct ids.
- **Word spans** pair a change block's i-th `del` with its i-th `add` and diff
  the pair token by token (words, whitespace runs, single punctuation). A pair
  where more than 60% changed is a rewrite and gets no spans; so does a line
  over 1000 chars.
- **Syntax spans.** syntect's parser on the pure-Rust `fancy-regex` engine,
  over bat's ~200-syntax set from `two-face` (TypeScript/TSX, Swift, Kotlin and
  TOML included — syntect's own default set lacks them). No theme ships; scopes
  fold onto the 17 `legend` kinds and each client maps those to its theme
  (Catppuccin Mocha on the Mac). Each hunk side is its own stream, so a hunk
  that opens inside a block comment can be miscolored until it closes. A line
  over 2000 bytes stops that stream; past 750 ms per diff lines arrive
  uncolored. Binary-size cost, measured on a stripped release binary: about
  1.3 MB. tree-sitter-highlight was the alternative: a C grammar crate per
  language, larger and not pure Rust.
- **Noise** (collapsed, hunks left out of the full frame): lockfiles (by
  name), vendored directories, generated files (`.generated.`, `*.pb.go`,
  `__generated__/`, `__snapshots__/`, …), minified files (`.min.js`, or added
  lines mostly over 500 chars), and anything over 1500 changed lines.

### Size caps — explicit, never silent

| Cap                     | Value   | Marker                                                              |
| ----------------------- | ------- | ------------------------------------------------------------------- |
| `git diff` output read  | 64 MiB  | `diff.truncated`                                                    |
| Files listed            | 1500    | `diff.truncated`, `files_omitted`                                   |
| Lines per file          | 3000    | `file.truncated`                                                    |
| Lines per hunk          | 1000    | `hunk.truncated` (+ `file.truncated`)                               |
| Chars per line          | 2000    | `line.truncated`                                                    |
| Serialized frame budget | 512 KiB | `hunks_omitted: "budget"` (full diff) / `file.truncated` (one file) |

The frame budget is for the relay: phones reach the engine through
`relay.smoo.ai`, URLSession's default WebSocket message limit is 1 MiB, and
end-to-end encryption adds a third in base64. Counts (`added`/`deleted`) are
always the real ones. A stubbed or collapsed file is fetched alone with
`flow.diff{path}` — **paging by file**.

## Protocol

All additive to v0.1; a client that ignores unknown types is unaffected. Each
request may carry a client `seq`, echoed as `flow.error.ref`.

| Client → engine                                      | Reply                                                         |
| ---------------------------------------------------- | ------------------------------------------------------------- |
| `flow.diff {id, base, path?}`                        | `flow.diff {id, base, path?, diff}` (direct, never broadcast) |
| `flow.diff.revert {id, base, hunk_id}`               | `flow.diff.result {id, action:"revert", hunk_id, file}`       |
| `flow.diff.stage {id, hunk_id, base?=uncommitted}`   | `flow.diff.result {…, action:"stage"}`                        |
| `flow.diff.unstage {id, hunk_id, base?=uncommitted}` | `flow.diff.result {…, action:"unstage"}`                      |
| `flow.diff.review {id, base, comments[]}`            | `flow.diff.result {id, action:"review", message}`             |

Broadcast: `flow.diff.changed {id}` after an `end` snapshot and after a hunk
action — a client showing that session's diff re-requests it.

Errors are `flow.error` with code `stale` (the hunk is gone or no longer
applies; nothing written), `blocked` (a review to an agent in `needs_you`,
`limited`, `done` or `dead`, or to a shell), `not_found`, or `failed`.

HTTP twins (daemon token required): `GET /api/flow/sessions/{id}/diff?base=&path=`,
`POST …/diff/revert {hunk_id, base}`, `POST …/diff/stage {hunk_id}`,
`POST …/diff/unstage {hunk_id}`, `POST …/diff/review {base, comments}` →
`{message}`. `stale` and `blocked` are 409.

A comment is `{file, hunk_id?, line_range?: [first, last], side?: "new"|"old", text}`.

## Hunk actions

The engine recomputes the base's diff, finds the hunk by id, writes a patch
with exactly that hunk, and runs `git apply --check` before `git apply`:

- **revert** — `git apply --reverse` on the worktree, any base. For `turn` on
  an idle agent the hunk's new side is the turn's end, so an edit of yours
  since then makes it `stale` rather than clobbering you.
- **stage** — `git apply --cached`, `uncommitted` only (the index is
  HEAD-relative). An untracked file stages as a new file.
- **unstage** — the hunk looked up in HEAD → index, `git apply --cached --reverse`.

A rename's hunk is written against the new path and a mode change is left
out: neither is a hunk. Binary files have no hunks to act on. `staged: true`
marks a hunk of the `uncommitted` view that is byte-identical in HEAD → index.

## Review → steer

`flow.diff.review` turns the batch into ONE message through `flow.send` (the
manifest's paste + submit), so the agent gets every comment in one turn:

````text
Code review of your last turn from SmoothFlow — 2 comments. Please address each one:

1. src/lib.rs:12-14
   ```diff
   -    let x = 1;
   +    let x = 2;
   ```
   Why change this?

2. README.md
   Fix the typo in the intro.
````

Excerpts come from the diff as it is when the review is sent (at most 12
lines, 200 chars each); a comment whose hunk has since changed says so.

## Phase 2 — what the desktop app and the phones need

- **Nothing new from the engine.** Everything is WS frames; the relay
  forwards any `channel:"flow"` frame, and the 512 KiB budget keeps one frame
  inside the phones' message limit after encryption.
- Decode `diff::model` (the field list above), replay `spec/vectors/diff.json`,
  and use the Client Spec §14 rules: side-by-side pairing, the file tree, the
  n/p and ]/[ order, collapsed state, viewed keys, default base.
- **Desktop (GPUI)** can depend on `smooth-flow-client` directly and, if it
  likes, on `smooth-flow`'s `diff::model` types for decoding.
- **Phones:** unified only below a width, the file list as the first screen,
  comments and Send Review as visible controls (phones have no shortcut layer).
  Revert must confirm.
- Not in phase 1: line-level staging, markdown/image previews, a staged view,
  persisted viewed marks.
