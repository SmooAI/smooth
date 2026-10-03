# smooth-flow-vt

The SmoothFlow session host's headless terminal (th-5025fb,
[ADR-011](../../docs/Decisions/ADR-011-smoothflow-engine-owned-ptys.md)): a safe
Rust wrapper around libghostty-vt. Every byte a session prints goes through one
`Vt`, which replaces what tmux did for the engine:

| tmux                               | `Vt`                                      |
| ---------------------------------- | ----------------------------------------- |
| `capture-pane -p`                  | `plain_screen()`                          |
| `capture-pane -p -S - -J`          | `plain_scrollback()`                      |
| `#{alternate_on}`, cursor, title   | `alternate_on()`, `cursor()`, `title()`   |
| `send-keys Enter` / `C-c` / `Down` | `encode_key(name)`                        |
| `paste-buffer -p`                  | `encode_paste(text)`                      |
| redraw on `attach`                 | `snapshot(max_bytes)` (the `flow.replay`) |
| answering DA / DSR queries         | `take_replies()`                          |

The module docs in `src/lib.rs` cover the snapshot format, how the
alternate screen keeps the primary screen's history, the byte-budget policy
and the known libghostty-vt limits.

## Building

`build.rs` links libghostty-vt from `scripts/ghostty-vt/build-ghostty-vt.sh`,
pinned by `scripts/ghostty-vt/ghostty-vt.lock`. SmoothFlow Desktop builds from
the same script and pin, and they share the ghostty commit with the Mac, iOS
and Android apps, so the daemon snapshots with the same parser every client
replays with. The script fetches a sha256-pinned Zig and builds the static
library once.

| Variable          | Effect                                                                                 |
| ----------------- | -------------------------------------------------------------------------------------- |
| `GHOSTTY_VT_DIR`  | Link a prebuilt `<dir>/{include,lib}`; the script never runs.                          |
| `GHOSTTY_VT_WORK` | The script's work dir (CI keeps it in the checkout so it can be cached).               |
| (neither)         | `<user cache>/smooth/ghostty-vt/<ghostty commit>`: one build shared by every worktree. |
| `GHOSTTY_VT_BASH` | The bash to run the script with (Windows defaults to Git Bash).                        |

The crate is a workspace member, so a workspace-wide `cargo build` or
`cargo test` builds it. `smooth-flow` depends on it only behind the `pty-host`
feature (off by default) until `PtyHost` lands. Until then, `-p smooai-smooth-flow`,
`-p smooai-smooth-daemon`, `-p smooai-smooth-cli` and their release builds don't
need Zig.

## Unsafe

The workspace forbids `unsafe_code`, and a forbid can't be relaxed further
down. So this crate copies the workspace lints with `unsafe_code = "deny"`,
and `src/ffi.rs` is the one module that allows it. `csrc/flow_vt.c` keeps every
libghostty struct on the C side, so Rust only passes ints and byte buffers.
