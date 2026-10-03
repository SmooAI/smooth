---
status: Accepted
date: 2026-10-03
pearl: th-ce4f88
---

# ADR-011 — SmoothFlow drops tmux: engine-owned PTYs with a headless libghostty-vt per session

## Status

Accepted (2026-10-03, epic th-ce4f88). Brent chose this over two tmux-only fixes
for th-e19c22.

## Context

Every SmoothFlow session runs in a long-lived `tmux -L smooth-flow` server, and
clients see it through a `tmux attach` client running on a `portable-pty`
([SmoothFlow.md § Process model](../Architecture/SmoothFlow.md#process-model)).
tmux gave us three things cheaply:

- sessions that outlive the daemon;
- a full redraw for a late-joining client;
- `capture-pane` for state scraping.

It also costs us:

- **No scrollback on any client (th-e19c22).** A tmux client draws on the
  alternate screen, so the client's libghostty-vt never accumulates history. On
  every client (Mac, desktop, iOS, Android) the wheel turns into arrow keys,
  and you can't scroll back through what an agent printed.
- **Lost output is lost.** A slow WS subscriber that lags the broadcast
  channel drops `flow.output` silently (`flow_route.rs`). The client has no way
  to resync short of re-attaching, and re-attaching only gets tmux's redraw of
  the visible screen.
- **The exit-status maze.** `remain-on-exit`, `pane_dead`, `run-shell true`
  nudges for missed SIGCHLDs, an `sh` wrapper writing `$?` to a file, and a 3 s
  "unknown" fallback all exist because tmux, not us, is the parent of the
  agent (th-7ff336, th-9d2578).
- **Byte-level hazards.** These include `portable-pty`'s writer typing
  `\n`+`^D` through the raw tmux client (th-6d8f84), tmux turning TABs into `_`
  without `LANG` (th-tmux-locale), and blank Nerd glyphs from a non-UTF-8
  attach client.
- **A runtime dependency.** tmux must be installed and found off-`PATH`
  (th-9f6814), and it doesn't exist on Windows at all.

We looked at three peers ([th-e19c22 comment](#references)). None of them uses
tmux by default:

| Project             | PTY owner                                             | Survives the owner restarting by                                                           | Snapshot model                                                                                      |
| ------------------- | ----------------------------------------------------- | ------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| cmux-tui (manaflow) | one long-lived **terminal host process per terminal** | the mux adopts the running host                                                            | headless ghostty-vt; `attach-surface` sends `vt-state` (a VT replay), then ordered `output` frames  |
| herdr               | one Rust server                                       | handing PTY fds to the new server over `SCM_RIGHTS` on update                              | ghostty-vt per pane; the server renders frames                                                      |
| Orca (stablyai)     | a detached Node daemon (node-pty)                     | the daemon outliving Electron, plus an on-disk output log and checkpoints for cold restore | `@xterm/headless` + SerializeAddon; snapshot includes `scrollbackAnsi` captured while alt screen on |

cmux's protocol doc also records why they dropped their earlier raw-byte
replay: a replay can start in the middle of an escape sequence and corrupt the
grid.

## Decision

The SmoothFlow engine owns its PTYs. Each session runs under a small
**session host process**. The host owns the PTY master, is the parent of the
agent, and keeps a **headless libghostty-vt terminal** that every output byte
passes through. A client attach is a **snapshot followed by sequenced live
bytes**. tmux leaves SmoothFlow entirely. `smooth-tmux` stays for `th claude`,
and its pure `detect` heuristics stay shared.

1. **Session host (`smooth-daemon flow-host`).** This is a subcommand of the
   binary we already ship, not a new binary in the bundle.
    - The daemon spawns one host per session, detached into its own process
      group and session (`setsid`), so it survives a daemon crash, restart or
      update.
    - The host listens on a per-session Unix socket
      (`~/.smooth/flow-hosts/<id>.sock`, mode 0600, under a 0700 directory) or,
      on Windows, a named pipe with an owner-only DACL.
    - It writes a host record (`<id>.json`: pid, start time, protocol version,
      socket path) next to the socket.
    - On boot the daemon adopts every live host it finds, instead of looking
      sessions up by tmux name.
    - Following cmux's model, the host carries no daemon-local state, so any
      daemon can adopt it.
2. **The host is the parent.** It `wait()`s on the agent, so the exit code is
   exact. The `sh` exit-code wrapper, the exit files, `pane_dead`, the
   `run-shell` nudge and the 3 s "unknown" fallback are all deleted. A dead
   session's host lingers with the exit status and final screen until the
   engine reads them (the old `remain-on-exit`), then exits.
3. **Headless VT and snapshot.**
    - The host feeds every byte into libghostty-vt at the pinned
      manaflow-ai/ghostty commit, the same one every client parses with
      (`ghostty-vt.lock`).
    - It keeps a monotonic per-session `seq`. Today's `seq` restarts per bridge.
    - A snapshot is libghostty-vt's VT formatter output: screen, history,
      cursor, SGR, modes, scrolling region, charsets and keyboard modes, tagged
      with the `seq` it is current through.
    - While a TUI is on the alternate screen, the snapshot carries the primary
      screen's history too (Orca's `scrollbackAnsi`), so scrollback survives
      full-screen agents.
    - Snapshots are bounded by byte budget, newest rows first, as in cmux's
      `vt_replay_bounded`. A pathological row falls back to a reset rather than
      a stuck attach.
4. **Protocol (additive, versioned).**
    - The engine answers `flow.attach` with a new `flow.replay{id, cols, rows,
seq, data_b64}`, then `flow.output{id, seq, data_b64}` frames with
      `seq` > the replay's.
    - A client resets its terminal, applies the replay, and drops any output
      whose `seq` is ≤ the replay's.
    - A subscriber that lags gets a fresh `flow.replay` instead of a silent gap.
    - A `flow.resize` that changes geometry is followed by a `flow.replay` at
      the new size.
    - Phones get the replay through the same 16 KiB / 30 fps coalescer, chunked.
    - Because the client terminal now stays on the primary screen, client-side
      scrollback simply works. Every client (Mac, GPUI desktop, iOS, Android,
      mock, `th flow attach`) lands this in the same epic, per the
      phones-first-class rule.
5. **Input without tmux.**
    - A paste is written to the PTY as bracketed paste (`ESC[200~ … ESC[201~`)
      only when the app has enabled mode 2004, which the host's VT knows, and
      as plain bytes otherwise.
    - Manifest key names (`Enter`, `C-c`, `Right`, …) map to bytes through one
      table in `smooth-flow`. The table honours the VT's cursor-key mode
      (DECCKM) and Kitty keyboard flags, using libghostty-vt's key encoder.
      tmux did this for us; now we do.
6. **Scraping without tmux.** `capture-pane` becomes the host's visible-screen
   text (formatter `PLAIN`), plus `alternate_on`, `cursor_y` and the OSC title,
   all read from the VT. `smooth_tmux::detect` is unchanged.
7. **Migration.**
    - `PtyHost` implements the existing `SessionHost` seam (th-64d4ab) and sits
      beside `TmuxHost`.
    - `SMOOTH_FLOW_HOST=pty|tmux` selects the host for new sessions. Each row
      records which host it lives on, so live tmux sessions finish on tmux.
    - The flow e2e suite runs against both hosts until the default flips. One
      release later, `TmuxHost` and the tmux paths in `smooth-flow` are removed.
    - The Mac app's private tmux server for TCC goes too. Hosts are descendants
      of the app-started daemon, so TCC attribution follows them the same way
      (see [SmoothFlow TCC](../Architecture/SmoothFlow.md#tmux-socket-tcc)), and
      quitting the app ends them, as `kill-server` does today.

## Reasoning

### Why a host per session rather than PTYs inside the daemon

A daemon that held the PTY masters itself would take every session down with
it on a crash, restart or update. Keeping sessions alive across those three is
the property tmux gave us, and we can't regress it.

herdr's `SCM_RIGHTS` handoff covers a planned update, but not a crash, and it
doesn't exist on Windows. Orca's cold-restore log brings back the screen, not
the process.

A per-session host covers all three cases on every OS. One misbehaving session
can't take its siblings down, and the daemon's job shrinks to adopting hosts
and fanning out frames. The cost is a process per session plus a small IPC
protocol, and that protocol must be versioned, because an old host can outlive
a newer daemon.

### Why libghostty-vt and not alacritty_terminal or our own serializer

- Every SmoothFlow client already parses with libghostty-vt at one pinned
  commit. A snapshot produced by that same parser replays exactly on every
  client.
- Its C API ships a VT formatter, so we don't have to maintain a grid-to-SGR
  serializer.
- `apps/smoothflow-desktop` already builds and links it on macOS, Linux and
  Windows (`build.rs`, `scripts/build-ghostty-vt.sh`, `csrc/smoothflow_vt.c`).
- The cost is a Zig toolchain in the daemon's build. The desktop build script
  already pins and fetches it, and CI caches it.

### Why snapshot-then-seq rather than raw byte replay

A raw byte log has to be capped. The cap then cuts mid-sequence, which is
cmux's documented failure. It also grows without bound for chatty agents, and
it replays history that the agent has since erased. A formatter snapshot is
the terminal's state, not its history, so it is bounded and always parseable.

### Why not keep tmux and fix it

The tmux-only options for th-e19c22 were `smcup@:rmcup@` overrides, which let
redraw noise into history, or `mouse on` with copy-mode, which breaks native
selection. Both fix one symptom and keep the rest:

- the exit maze;
- lossy lag;
- the runtime dependency;
- no Windows support.

## Implementation

Sub-pearls under th-ce4f88, in order: th-c61966, th-5025fb, th-e4aef9, th-dc9822, th-cbb0af, th-68234b. Each step lands green before the next
one starts.

1. **ADR and spec.** This document. Then the `flow.replay` frame, per-session
   `seq`, the lag-resync rule and the host IPC protocol go into
   `SmoothFlow.md`, `SmoothFlow-Client-Spec.md` and `mock/protocol.md`, with
   new `spec/vectors` cases (attach, replay, then output ordering; stale-seq
   drop).
2. **`smooth-flow-vt`.** A safe Rust wrapper around libghostty-vt for the
   daemon: feed, resize, plain screen text, mode queries, a bounded VT
   snapshot, and key and paste encoding. It reuses the desktop's pinned build
   script; the desktop app moves onto this crate later.
    - Exhaustive tests. Round-trip: feed bytes, snapshot, feed the snapshot
      into a fresh terminal, and the screens and history are equal.
    - The alternate-screen history case, the budget edge, and adversarial
      input.
3. **Session host.** `smooth-daemon flow-host`, built on `portable-pty` and
   ConPTY.
    - Child wait, the IPC server, host records, and adoption on daemon boot.
    - Linger-after-exit.
    - Tests cover a daemon kill followed by re-adoption with the session intact.
4. **`PtyHost: SessionHost`.**
    - Launch, input, resize, paste and keys through the table.
    - Scrape from the VT.
    - Exit status from `wait`.
    - Tree kill.
    - It sits behind `SMOOTH_FLOW_HOST`, with a `host` column on `sessions`.
5. **Engine and clients.**
    - The engine sends `flow.replay` on attach, after a lag and after a resize.
    - The relay chunks the replay.
    - The Mac (Swift), GPUI desktop, iOS, Android, mock and `th flow attach`
      clients consume it.
6. **Flip and delete.**
    - The flow e2e suite runs green on `pty`.
    - Then the default flips, and the Mac app's tmux server goes.
    - One release later, `TmuxHost` and the flow-side tmux code are deleted, and
      th-e19c22 closes.

## References

- th-e19c22 comment (2026-10-03): the cmux, Orca and herdr survey, with file
  references. The local clones are in `~/dev/refs/{cmux,orca}`; herdr is
  `github.com/herdrdev/herdr`.
- cmux is used as a design reference only, and no code is copied. Its root
  license is GPL-3.0-or-later.
- [SmoothFlow.md § The session host seam](../Architecture/SmoothFlow.md#session-host)
- libghostty-vt `formatter.h` at the pinned commit in
  `apps/smoothflow-desktop/ghostty-vt.lock`.
