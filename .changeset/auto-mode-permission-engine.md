---
'@smooai/smooth': minor
---

Add the Claude-Code-style auto-mode permission engine (pearl th-515a13) — the
primary tool-execution enforcement layer now that the microVM stack
(Wonk/Goalie, PR #124) is gone.

A new `ToolHook` (`smooth-bigsmooth/src/auto_mode.rs`) is added FIRST on the
operative's tool registry, so its verdict gates before Narc and before the tool
runs. Every tool call gets an **allow / deny / ask** verdict from a pure,
exhaustively-tested `decide()`:

- **Modes** via `SMOOTH_AUTO_MODE`: `ask` (default — read-only allow, mutating
  ask, dangerous deny), `accept-edits` (filesystem-edit tools auto-approve),
  `deny` (headless/CI — unmatched ask becomes deny, fail-closed), and `bypass`
  (allow all except the hard circuit-breakers).
- **Layered policy**: credential-path guard (deny read *and* write, survives
  bypass) → baseline dangerous-CLI / dangerous-domain deny (reuses
  `smooth_narc::judge`) → `wonk-allow.toml` allow-lists (user + project,
  project-wins) → compiled-in read-only default posture. Precedence is
  deny > ask > allow. Compound commands are split on `&&`/`||`/`;`/`|`/`&`/
  newlines so a safe first command can't smuggle a dangerous one; wrapper
  prefixes (`timeout`, `env`, …) are stripped first.
- **Ask channel**: an `Ask` verdict blocks on the shared `AccessStore`, surfaced
  via the existing `/api/access/{pending,approve,deny,stream}` routes + TUI, and
  **fails closed** on timeout/headless (default 300s). Approvals persist at the
  chosen scope (`Once` / `Session` / `PearlProject` / `User`) into the
  corresponding `wonk-allow.toml`.

Docs: `docs/Engineering/Auto-Mode-Permissions.md` + a CLAUDE.md security note.
