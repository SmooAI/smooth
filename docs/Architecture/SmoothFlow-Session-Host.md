# SmoothFlow session host — the daemon ⇄ `flow-host` protocol

> Epic th-ce4f88, spec pearl th-c61966. Decision:
> [ADR-011](../Decisions/ADR-011-smoothflow-engine-owned-ptys.md). Engine and
> client wire: [SmoothFlow.md](SmoothFlow.md). Status: **specified, not yet
> built** (implementation is th-e4aef9 and th-dc9822).

Every SmoothFlow session on the `pty` host runs under its own
`smooth-daemon flow-host` process. The host owns the PTY master, is the
agent's parent, and feeds every output byte through a headless libghostty-vt
terminal. The daemon talks to it over a private local socket with the
protocol below. The daemon is a **client** of the host: the host outlives
daemon crashes, restarts and updates, and any later daemon adopts it.

The protocol is versioned from its first message, because a host started by
an old daemon routinely outlives it and is adopted by a newer one.

## Files and transport

| Item        | Unix                                                    | Windows                                                     |
| ----------- | ------------------------------------------------------- | ----------------------------------------------------------- |
| Host dir    | `~/.smooth/flow-hosts/`, mode **0700**                  | `%USERPROFILE%\.smooth\flow-hosts\`, owner-only DACL        |
| Endpoint    | `<dir>/<id>.sock`, Unix stream socket, mode **0600**    | named pipe `\\.\pipe\smooth-flow-host-<user-sid-hash>-<id>` |
| Host record | `<dir>/<id>.json`, mode **0600**                        | `<dir>\<id>.json`, owner-only DACL                          |
| Override    | `$SMOOTH_FLOW_HOSTS_DIR` (tests point it at a temp dir) | same                                                        |

- `<id>` is the flow session id (`fs-` + 8 hex).
- The daemon creates the directory with 0700 before the first spawn and
  refuses to use one that is not owned by the current user or is group- or
  world-accessible.
- The host binds the socket with umask 077 and then `chmod`s it to 0600, so
  there is no window where it is wider.
- A Unix socket path is limited to about 104 bytes on macOS. When
  `<dir>/<id>.sock` is longer than that, the host binds
  `$TMPDIR/smooth-flow-<uid>/<id>.sock` instead (same 0700/0600 rules). The
  daemon **always reads the socket path from the record** and never derives
  it.
- The Windows pipe is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
  `PIPE_REJECT_REMOTE_CLIENTS`, and a DACL that grants access to the current
  user's SID only.

## Authentication

Two layers, both required:

1. **Filesystem permissions.** Only the owning user can reach the socket or
   the pipe (above). On Unix the host also checks the peer's uid
   (`SO_PEERCRED` on Linux, `getpeereid` on macOS) and closes a connection
   from any other uid before reading a byte.
2. **A per-host token.** The daemon generates 32 random bytes (hex) per spawn
   and passes them to the host on stdin, never in argv or the environment, so
   they don't show in `ps`. The host writes the token into its 0600 record,
   which is how a later daemon learns it. Every connection must present it in
   `hello`; the host compares in constant time and answers a mismatch with
   `error{code:"auth"}` and a close.

## Spawn

The daemon runs `smooth-daemon flow-host --id <id>` (the binary it is itself)
with a single JSON object on stdin, then closes stdin:

```json
{
    "token": "<64 hex>",
    "dir": "/Users/me/.smooth/flow-hosts",
    "argv": ["claude", "--session-id", "…"],
    "cwd": "/Users/me/dev/x",
    "env": { "LANG": "en_US.UTF-8", "PATH": "…" },
    "cols": 120,
    "rows": 40,
    "seq_start": 0,
    "scrollback_rows": 10000,
    "linger_secs": 86400,
    "owner": "smoothflow"
}
```

- The host detaches first: a new session and process group (`setsid` on Unix,
  `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on Windows), stdio pointed at
  `/dev/null` (or `NUL`) once the handshake below is done, and its working
  directory set to `/`. A daemon crash then can't take it down.
- It binds the endpoint, writes the record (temp file + rename), spawns the
  child on a fresh PTY (`portable-pty`; ConPTY on Windows) with its own
  process group (Windows: a job object), and prints one line on stdout:
  `{"ready":true,"socket":"<path>","pid":<host pid>}`. The daemon waits up to
  5 s for it, then connects. No line, or `{"ready":false,"error":"…"}`, sends
  the session row to `dead` with attention `launch_failed` and the error as
  its detail, as tmux launch failures do today.
- `env` is the complete environment. If it has no UTF-8 `LANG`/`LC_ALL`, the
  host sets `LANG=en_US.UTF-8` (the blank-glyph lesson from tmux).
- `seq_start` is where this host's output `seq` begins. A relaunch (Kill &
  Resume, crash resume) passes the previous host's final `seq` + 1, so a
  session's `seq` keeps rising across hosts. Clients don't depend on that
  (the latest replay is always their baseline), but logs and debugging do.

## Host record (`<id>.json`)

```json
{
    "v": 1,
    "protocol": 1,
    "id": "fs-1a2b3c4d",
    "host_version": "0.52.0",
    "pid": 41234,
    "pid_start": "Sat Oct  3 17:40:12 2026",
    "child_pid": 41235,
    "socket": "/Users/me/.smooth/flow-hosts/fs-1a2b3c4d.sock",
    "token": "<64 hex>",
    "owner": "smoothflow",
    "cwd": "/Users/me/dev/x",
    "argv": ["claude", "--session-id", "…"],
    "created_at": "2026-10-03T17:40:12Z",
    "exit": null
}
```

- `v` is the record schema; `protocol` is the IPC version this host speaks
  (fixed for its lifetime).
- `pid` + `pid_start` are the liveness index, the same pair the engine stores
  for tmux panes today (`ps -o lstart=` on Unix, the process creation time on
  Windows), so a recycled pid can't pass for the host.
- `owner` is the creating daemon's ownership identity (th-4f7866), so only
  that daemon's supervision adopts it.
- `exit` is `null` while the child runs. When it exits the host rewrites the
  record with `{"code": <i32|null>, "signal": <i32|null>, "at": "<rfc3339>"}`
  before notifying, so a daemon that was down at the time still learns the
  exact status.
- Every write is temp file + rename. The host deletes its record and socket
  as the last thing it does before exiting.

## Framing

A message is:

```
u32 BE  total length of what follows (≤ 16 MiB)
u32 BE  header length H (≤ 64 KiB)
H bytes header: UTF-8 JSON object with a "type" field
rest    body: raw bytes (may be empty)
```

Bulk bytes (PTY output, input, snapshots, screen text) travel in the body,
never base64 inside JSON. A frame over either limit is a protocol error: the
receiver sends `error{code:"frame_too_large"}` and closes.

Requests that expect an answer carry `req` (a u64 chosen by the daemon), and
the answer echoes it. Everything else is fire-and-forget.

## Handshake and versioning

The daemon speaks first, and nothing else may be sent before the handshake
completes.

```
daemon → host  {"type":"hello","protocol":[1,1],"token":"…","client":"smooth-daemon/0.52.0"}
host → daemon  {"type":"hello","protocol":1,"host_version":"0.52.0","id":"fs-…",
                "pid":41234,"child_pid":41235,"cols":120,"rows":40,"seq":812,
                "running":true,"exit":null}
```

- The daemon offers the inclusive range of protocol versions it speaks. The
  host picks the highest it also speaks and answers with it, or answers
  `error{code:"version", "message":"host speaks 1..1"}` and closes.
- `seq` is the last output `seq` the host has produced, and output frames
  sent after the hello have `seq` greater than it.
- **Compatibility window.** A daemon must speak every protocol version a host
  from the previous two minor releases may speak. A daemon that meets a host
  it can't speak to **leaves it running** and marks the row with attention
  `held` and detail `host speaks protocol N; this daemon speaks A..B`. It can
  still kill it without the protocol, by signalling `pid` from the record
  after checking `pid_start`.
- **Additive changes** (a new optional header field, a new message the other
  side may ignore) don't bump the version. A receiver ignores unknown header
  fields and answers an unknown `type` with `error{code:"unknown_type"}`
  without closing. A change that a peer must understand bumps the version.

## Connections

The host serves **one daemon connection at a time**. A new connection that
completes the handshake replaces the current one: the host sends the old one
`error{code:"superseded"}` and closes it. This makes adoption after a daemon
crash work even when the dead daemon's socket is still half-open. Two daemons
never fight over a host because only the record's `owner` adopts it.

Liveness: either side may send `ping{req}`; the other answers `pong{req}`.
The daemon pings every 10 s and treats 3 missed pongs as a dead connection
(it reconnects; it does not assume the session died).

## Messages, daemon → host

| `type`     | Header fields                                | Body       | Effect                                                                                                                                                                                                                             |
| ---------- | -------------------------------------------- | ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `input`    | —                                            | bytes      | Written to the PTY as-is (keyboard and mouse encodings from a client's `flow.input`).                                                                                                                                              |
| `paste`    | —                                            | UTF-8 text | Encoded by `Vt::encode_paste`: wrapped in `ESC[200~ … ESC[201~` when the VT has mode 2004 on, else with newlines sent as CRs. ESC, NUL, DEL and similar control bytes become spaces either way, so a paste can't end itself early. |
| `key`      | `name`, `repeat?`                            | —          | A manifest key name (`Enter`, `C-c`, `Right`, `S-Tab`, …) encoded through libghostty-vt's key encoder under the VT's current cursor-key mode (DECCKM) and Kitty keyboard flags. Unknown names answer `error`.                      |
| `resize`   | `req`, `cols`, `rows`                        | —          | Resizes the PTY (SIGWINCH) and the VT. Answered `resized{req, seq, changed}`; `changed` is false when the size was already that.                                                                                                   |
| `snapshot` | `req`, `max_bytes`                           | —          | Answered `snapshot{req, seq, cols, rows, alternate, fidelity}` with the VT snapshot in the body.                                                                                                                                   |
| `screen`   | `req`                                        | —          | Answered `screen{req, seq, cols, rows, alternate_on, cursor_x, cursor_y, title, modes}` with the visible screen as plain text in the body (formatter `PLAIN`). This replaces `capture-pane` for state scraping.                    |
| `kill`     | `req`, `signal` (`term`\|`kill`), `grace_ms` | —          | Signals the child's process group (`killpg`; on Windows, the job object), and after `grace_ms` (default 3000) sends `kill` if it is still alive. Answered `ok{req}`; the `exit` notification follows.                              |
| `release`  | —                                            | —          | Valid only after `exit`: the daemon has stored the status and final screen. The host deletes its record and socket and exits 0.                                                                                                    |
| `ping`     | `req`                                        | —          | Answered `pong{req}`.                                                                                                                                                                                                              |

`modes` in a `screen` answer is
`{bracketed_paste, cursor_keys_app, mouse_tracking, mouse_format, alt_scroll,
kitty_keyboard}`, all read from the VT. `title` is the last OSC 0/2 title, or
`null`.

## Messages, host → daemon

| `type`    | Header fields                                 | Body  | Meaning                                                                                                                                                                           |
| --------- | --------------------------------------------- | ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `output`  | `seq`                                         | bytes | PTY bytes, already fed to the VT. `seq` rises by 1 per message.                                                                                                                   |
| `overrun` | `through_seq`                                 | —     | The daemon fell behind and the host dropped queued output through `through_seq`. The daemon must request a `snapshot` and send every attached client a `flow.replay`.             |
| `exit`    | `code`, `signal`, `seq`                       | —     | The child exited. `code` is its exit status or `null`; `signal` is the terminating signal or `null`. `seq` is the final output `seq`. The record already carries the same `exit`. |
| `error`   | `req?`, `code`, `message`                     | —     | `auth`, `version`, `superseded`, `frame_too_large`, `unknown_type`, `bad_request`, `not_exited`, `unknown_key`.                                                                   |
| answers   | `resized`, `snapshot`, `screen`, `ok`, `pong` | —     | As in the table above.                                                                                                                                                            |

## Output, `seq` and snapshots

- The host reads the PTY on one thread. For each read it feeds the bytes to
  the VT, assigns the next `seq`, and queues an `output` message, in that
  order. A snapshot taken between two reads is therefore current through
  exactly the last assigned `seq`.
- The host never blocks a PTY read on the daemon. It queues up to 8 MiB for
  the connection; past that it drops the queue and sends `overrun`. With no
  daemon connected it queues nothing: output still reaches the VT, and the
  next daemon starts from a snapshot.
- **The snapshot** is `smooth_flow_vt::Vt::snapshot`: libghostty-vt's VT
  formatter output (screen, history, cursor and pen, modes, margins, tab
  stops, charsets, keyboard modes, pwd, and the OSC 2 title when it fits),
  never larger than `max_bytes`. It degrades in this order: everything; else
  the full screen plus the newest history rows that fit (never cut mid-row
  or mid-escape); else the visible screen's plain text and cursor; else
  empty. The `snapshot` answer carries that `fidelity`
  (`full`|`history`|`plain`|`empty`) in place of a bare `truncated`.
- **Rows, not lines.** Rows go out as laid out, soft wraps not rejoined, so
  every resize must be followed by a fresh snapshot (the engine sends one to
  every attached client).
- **Terminal queries.** After every `feed`, the host writes
  `Vt::take_replies()` (answers to DA, DSR and similar queries) back to the
  PTY. The host is the session's terminal; clients don't answer.
- **The alternate screen.** The formatter only covers the active screen. So
  when the host sees `?1049h`, `?1047h` or `?47h` (splitting the read at the
  sequence, and carrying a partial escape sequence across reads), it caches a
  VT snapshot of the primary screen at that instant; the primary can't change
  while the alternate screen is active. A snapshot taken on the alternate
  screen is `<primary snapshot> <the alt-enter sequence the program used>
ESC[H ESC[2J <alt snapshot without it>`. Without the `ESC[H ESC[2J` the
  alternate content lands on the wrong rows, and sending the enter sequence
  twice would save the wrong cursor. The alt screen gets the budget first and
  the cached primary the rest. This is how scrollback survives full-screen
  agents (th-5025fb; the crate docs in `crates/smooth-flow-vt/src/lib.rs` are
  the reference).

## Exit and linger

- The host `wait()`s on the child, so the status is exact: no wrapper, no
  exit file, no `pane_dead`, no unknown fallback.
- On exit it drains the PTY until EOF (so the final screen is complete),
  rewrites the record with `exit`, and sends `exit`. Then it **lingers**: it
  keeps answering `snapshot` and `screen` so the engine can read the final
  screen, and exits on `release`.
- With no daemon connected, a lingering host exits by itself after
  `linger_secs` (default 24 h). The record keeps the exit status until it does,
  and a daemon that boots in that window still settles the row from it.
- A host whose child is still running never exits on its own.

## Adoption on daemon boot

For every `*.json` in the host dir:

1. Parse it. An unreadable record, or one whose `owner` isn't this daemon's,
   is left alone.
2. Check that `pid` is alive with the recorded `pid_start`. If it isn't, the
   host died without cleaning up: delete the record and socket. If the row is
   not yet `done`/`dead`, settle it from the record's `exit` when present, else
   as **exit status unknown** (never auto-resumed, as today).
3. Otherwise connect and handshake. On success the session is live again:
   the engine requests a `snapshot` for any client that attaches, and sends
   `release` right away to a host that reports `running:false` once it has
   stored the exit.
4. A record with no matching row in this daemon's `flow.db` is left alone
   (another daemon's db, e.g. `th up` beside the app).

The daemon writes nothing about a host into `flow.db` except the row's `host`
column (`pty`); everything it needs to adopt the host is in the record. That
is what makes any daemon able to adopt any of its owner's hosts.

## What the daemon does with it

| Engine operation (`SessionHost`) | Host messages                                                                          |
| -------------------------------- | -------------------------------------------------------------------------------------- |
| launch                           | spawn, then `hello`                                                                    |
| attach stream                    | the connection's `output` stream, fanned out as `flow.output`                          |
| `flow.attach` / lag              | `snapshot` → `flow.replay`                                                             |
| `flow.resize`                    | `resize`, then `snapshot` → `flow.replay` to every attached client                     |
| `flow.input`                     | `input`                                                                                |
| `flow.send`                      | `paste` then `key{name:"Enter"}`                                                       |
| manifest keys                    | `key`                                                                                  |
| capture (state scraping)         | `screen`; `smooth_tmux::detect` reads the text, `alternate_on`, `cursor_y` and `title` |
| exit status                      | `exit`, or the record's `exit` after a restart                                         |
| kill / tree kill                 | `kill`                                                                                 |
| liveness                         | the connection plus `pid` / `pid_start`                                                |

## Testing (when built)

- Round-trip every message through the framing, including both size limits
  and a body split across reads.
- Handshake: version negotiation both ways, a bad token, a wrong-uid peer, a
  superseding connection.
- `seq` and snapshot agreement under concurrent output.
- Kill the daemon (SIGKILL) mid-stream, boot a new one, adopt, and get the
  same screen and an increasing `seq`.
- Exit while no daemon is connected: the next daemon settles the row from
  the record.
