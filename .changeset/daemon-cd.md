---
"@smooai/smooth": minor
---

Big Smooth: runtime working-directory scoping (`/cd`, `/pwd`, and a `cd` tool).

The daemon boots with a broad workspace root (`SMOOTH_WORKSPACE`, e.g. `~/dev`);
a conversation can now narrow itself at runtime so the file tools
(read/list/grep/write/bash) operate under a specific subdirectory.

- **Session-scoped cwd** (`smooth-tools::SessionCwd`): a per-conversation current
  directory, keyed by the operator's `conversation_id`, confined **under the
  root**. `set` canonicalizes the target + root and rejects `..` traversal and
  symlink escapes — a `/cd` can never point Big Smooth outside its sandbox. Unset
  ⇒ the root; two conversations get independent cwds.
- **`cd` tool** (`smooth-tools::CdTool`): the agent scopes itself when the user
  says "work on the smoo-hub repo". Injected per-turn by the daemon's
  `SandboxedToolProvider`, which resolves the conversation's cwd, confines the
  fs/grep/bash tools to it, and bakes the current dir into the tool description
  so the model always knows where it is.
- **`/cd <path>` + `/pwd`** in the smooth-web chat UI: handled UI-side (the
  operator's LocalServer owns the WS message path, so slash commands can't be
  intercepted server-side) via a new daemon route `GET`/`POST /api/session/cwd`
  that reads/writes the SAME store the `cd` tool uses. `/cd` with no arg or `~`
  resets to the root; results echo as a system line.
