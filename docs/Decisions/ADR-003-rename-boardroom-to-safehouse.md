---
status: Accepted
date: 2026-05-18
deciders: Brent
supersedes: None
superseded-by: None
tags: [decision, naming]
---

# ADR-003 — Rename "The Boardroom" to "The Safehouse"

#decision

## Status

Accepted (2026-05-18)

## Context

Before [[ADR-001-Consolidate-into-one-microVM|ADR-001]], Smooth ran Big Smooth + the cast in one microVM ("the Boardroom") and dispatched per-operator microVMs as siblings. The Boardroom was a corporate-coded name that fit the multi-VM topology — a room of directors managing the operators next door.

ADR-001 collapsed that into a single microVM containing the entire cast + operator-runner subprocesses. The name kept lingering, but its motivation didn't:

1. There is no second "room" any more — there's one VM, full stop. The "board" implication of multiple parties meeting is gone.
2. The rest of the cast was always heist/mob-coded: **Big Smooth** is the boss who never gets his hands dirty (READ-ONLY invariant, enforced by Narc), **Narc** is the rat / informer, **Bootstrap Bill** is the fixer, **Wonk** is the policy nerd, **Goalie** is the muscle on the door, **Scribe** keeps the books, **Smooth Operators** are the made men. Calling the place they all work out of a "Boardroom" was the one element that didn't fit the family.
3. Marketing copy and docs that lean into "the crew" / "the heist" land better than the corporate framing.

## Decision

Rename "the Boardroom" to "the Safehouse" throughout the codebase, env surface, OCI image artifacts, and documentation.

- **Code identifiers:** `Boardroom*` → `Safehouse*`, `boardroom_*` → `safehouse_*`, `BOARDROOM_*` → `SAFEHOUSE_*`.
- **File names:** `crates/smooth-bigsmooth/src/bin/boardroom.rs` → `safehouse.rs`, `boardroom_narc.rs` → `safehouse_narc.rs`, `tests/boardroom_e2e.rs` → `safehouse_e2e.rs`, `docker/Dockerfile.boardroom` → `Dockerfile.safehouse`, `scripts/build-boardroom*.sh` → `build-safehouse*.sh`.
- **Env vars:** `SMOOTH_BOARDROOM_MODE` / `_PORT` / `_IMAGE` → `SMOOTH_SAFEHOUSE_*`. `SMOOTH_BOARDROOM_IMAGE` remains accepted as a fallback in the CLI's env resolution to avoid breaking ad-hoc scripts during the transition.
- **OCI image:** the published `ghcr.io/smooai/boardroom:latest` remains the default image until the registry repo is republished as `safehouse`. The CLI's bind-mount overlay places a freshly cross-compiled `safehouse` binary at the image's legacy `/opt/smooth/bin/boardroom` entrypoint, so the rename is complete at runtime today — only the image tag string still reads "boardroom" until the next image republish.
- **Docs:** the current-state pages in `docs/Architecture/`, `docs/Operations/`, `docs/Start-Here/`, the white paper, and `README.md` are rewritten with "the Safehouse". `ADR-001` and `ADR-002` keep their original "Boardroom" wording — they're historical snapshots and the older name is correct in their context.

## Reasoning

### Why a heist/mob word and not the obvious alternatives

We considered:

- **The Stage** — fits the existing "Cast" metaphor (theatrical), but the family is closer to *Goodfellas* than *Hamilton*.
- **The Joint** — strong mob slang, doubles as a software term ("joint runtime"). Generic, but loses the heist-specific feel.
- **The Front** — best descriptive match (the microVM literally is a front: a clean HTTP port hiding the crew). Felt slightly too clever; the architecture's value isn't really "deception."
- **The Office** — Sopranos-coded, but the TV-sitcom collision is unavoidable and "office" still leans corporate.

**The Safehouse** wins because it's heist-coded, singular, evokes the right tradeoffs (a secure place where the crew operates while planning and pulling jobs), and the microsandbox hardware isolation maps cleanly onto the metaphor — the Safehouse is where the family is safe from the heat. Big Smooth runs the crew from the Safehouse; the Operators leave the Safehouse to pull jobs (read/write the workspace) and come back to report.

### Why not a corporate-friendly name

The product is consumer-developer-facing. The heist framing reads better in marketing copy ("Big Smooth runs the Safehouse; the operators run the jobs"), in TUI status lines, in tracing ("safehouse: cast spawned"), and in oncall docs. Corporate framing ("Headquarters", "Office", "Hub") would weaken the rest of the cast's naming.

## Consequences

### Positive

- Naming is internally consistent. Every cast member's metaphor lands in the same crime-family bucket now.
- Marketing/docs/CLI tracing read as one coherent product instead of "a corporate boardroom hiring a crew of mobsters."
- `SMOOTH_SAFEHOUSE_MODE=1` reads as a more specific dispatch decision than `SMOOTH_BOARDROOM_MODE` did — the env var name now describes *what kind of place we're in* (a sealed safehouse, not an open conference room).

### Negative / accepted

- Diff churn: ~70 files touched. Mitigated by the rename being a pure text substitution with no logic changes. Build is clean; the single failing lib test (`test_add_comment_and_get_comments`) is pre-existing flake [[Pearl|th-da2461]], unrelated to the rename.
- OCI image is still tagged `ghcr.io/smooai/boardroom:latest`. The CLI looks up `SMOOTH_SAFEHOUSE_IMAGE` first and falls back to `SMOOTH_BOARDROOM_IMAGE`, and the runtime overlay binds the new `safehouse` binary at the image's legacy `/opt/smooth/bin/boardroom` entrypoint, so this is invisible at runtime — but the registry rename is a follow-up.
- Pearls / changesets / ADR-001 / ADR-002 referencing "boardroom" stay as-is. Historical names should not be retroactively edited; they describe what was true when written.

### Reversal

Pure text substitution. If a future ADR wants to back out, sed -i 's/safehouse/boardroom/g; s/Safehouse/Boardroom/g; s/SAFEHOUSE/BOARDROOM/g' across the same file set, plus file renames, plus drop the legacy env-var fallback.

## Related

- [[ADR-001-Consolidate-into-one-microVM]] — the consolidation that made the old name obsolete
- [[ADR-002-microsandbox-0.4.6-and-remove-docker-backend]] — the version bump that landed alongside the consolidation cleanup
- [[../Architecture/Architecture-Overview]]
- [[../Architecture/The-Cast]]
- [[../Architecture/Sandboxed-Mode]]
- [[../Architecture/Transport]]
