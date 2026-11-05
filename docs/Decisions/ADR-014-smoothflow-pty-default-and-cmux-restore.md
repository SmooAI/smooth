---
status: Accepted
date: 2026-10-27
pearl: th-078a65
jira: SMOODEV-4238
---

# ADR-014 — SmoothFlow: the pty host is the default, and quitting the app keeps sessions

#decision

**Date**: 2026-10
**Status**: Accepted. Supersedes the last clause of [[ADR-011-smoothflow-engine-owned-ptys]] § Decision item 7 ("quitting the app ends them").

---

## Context

[[ADR-011-smoothflow-engine-owned-ptys]] added the `pty` host beside tmux and
planned to flip the default once the flow suite passed on both hosts. Two
things kept it on tmux:

- **Gaps against tmux.** A client terminal answered the program's terminal
  queries a second time on `pty` sessions, so stray DA/DSR bytes showed up
  as input. `⌘N` prewarm and the live tab title were tmux-only, and the app
  had no saved layout.
- **The quit story.** The Mac app kills its `smoothflow` tmux server on quit
  ([SmoothFlow macOS § TCC](../Architecture/SmoothFlow-macOS.md#daemon-lifecycle-and-tcc--the-part-cmux-gets-wrong)).
  The intent was that quitting the console restarts the fleet, so no process
  ever runs without the app's TCC attribution.

Brent asked for cmux-style restore. After an app or daemon restart, or a
reboot:

- every tab and split comes back in the same layout;
- live sessions are re-adopted as they are;
- dead agents resume through their harness, in the same cwd and session;
- plain shells reopen in their last cwd;
- nothing silently disappears;
- **quitting the app never kills a session.**

---

## Decision

1. **New sessions default to the `pty` host.**
    - `SMOOTH_FLOW_HOST` unset, empty or unknown selects `pty`
      (`HostKind::NEW_SESSION_DEFAULT`).
    - `SMOOTH_FLOW_HOST=tmux` still selects tmux.
    - A build without the `pty-host` feature (`cargo install`, `th up`,
      Windows) falls back to tmux with one warning, as before.
    - Rows keep the host they were created on. Existing tmux sessions finish
      on tmux, and a row with no `host` column still reads as tmux.
2. **Quitting the app never ends `pty` sessions.**
    - Each host is its own `setsid` process. The app's process-group kill on
      quit does not reach it, and the engine never stops a `pty` host on
      shutdown.
    - The app-owned tmux server is still killed on quit, but it only holds
      rows created before this change.
3. **The engine drops terminal replies on `pty` sessions.**
    - On a `pty` session, a `flow.input` frame made entirely of terminal
      replies (`replies::is_reply_only`) is dropped. The host's headless VT
      already answered those queries.
    - The Mac app also stops answering: a `pty` surface uses
      `GHOSTTY_SURFACE_IO_MANUAL_MIRROR`. The engine guard covers phones and
      older apps.
4. **The app persists its layout.** Tabs, split trees, divider fractions,
   focus, zoom and which session each pane shows are saved to defaults
   (`surfaceLayout`) and restored on launch. A pane whose session is gone
   comes back empty. It is never dropped.
5. **Restore is per session, by what is left of it:**

    | After the restart                     | What happens                                                                         |
    | ------------------------------------- | ------------------------------------------------------------------------------------ |
    | the host is still running             | re-adopted as it is (`adopt_existing`), with screen and scrollback intact            |
    | an agent's host is gone (reboot)      | relaunched through its harness resume option, in the same cwd and session id         |
    | a plain shell's host is gone (reboot) | reopened in its last sampled cwd (`last_cwd`), falling back to the worktree          |
    | none of those apply                   | the row stays visible with its exit status; it is never deleted on the user's behalf |

---

## Reasoning

### Survival over TCC attribution

The [TCC matrix](../Architecture/SmoothFlow-macOS.md#tcc-matrix-measured-2026-09-07-macos-264-developer-id-signed-ad-hoc-equivalent-build)
measured what happens after the app quits:

- a process the app started loses the app as its responsible process;
- re-adopting it on relaunch does not give that attribution back.

So a session that outlives a quit runs on its own TCC rows from then on. A
Full Disk Access or Automation request from it is attributed to its own
binary, not to SmoothFlow.

We accept that. Losing a running agent, or its unsent context, on every quit
costs more than a possible TCC re-prompt. The fix is cheap: restart the
session from the app. A harness resume relaunches it under the running app
and re-attributes it.

A daemon restart and an app update are unaffected, because the app stays the
responsible process.

### Why the engine filters replies instead of only the clients

Clients already in the field keep answering queries. The phones and SmoothFlow
Desktop both run on terminals that write a reply as its own write, so a reply
never shares a frame with typed bytes, and a whole-frame match is safe.

Only sequences a keyboard, mouse or paste never produce count as replies. The
one overlap is a cursor report on row 1, columns 2–16, which collides with a
modified F3. That case is let through.

### Why defaults for the layout

The layout is the console's view state, not engine state. Other clients (the
phones, the web) show their own views of the same sessions. Keeping it with
the app means a second Mac does not inherit this one's splits.

---

## Implementation

- `crates/smooth-flow/src/host.rs`: `HostKind::NEW_SESSION_DEFAULT`, `from_env_value`.
- `crates/smooth-flow/src/replies.rs`, plus the guard in `Engine::input`.
- `apps/smoothflow/Sources/UI/SavedLayout.swift`, `CenterViewController`, `AppController.surface(for:)`, `TerminalSurfaceView.ioMode`.
- Restart resume of dead agents is the restore engine from SMOODEV-4113 (boot id, clean-quit marker, staggered resume).

## Related

- [[ADR-011-smoothflow-engine-owned-ptys]]
- [[../Architecture/SmoothFlow]]
- [[../Architecture/SmoothFlow-macOS]]
