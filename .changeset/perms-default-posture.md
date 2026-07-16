---
"@smooai/smooth": minor
---

Big Smooth auto-mode now ships an **allow-benign, deny-dangerous** default posture
instead of the fail-closed all-`ask` default.

On first run with no `~/.smooth/permissions.toml`, the daemon writes a documented
starter policy (`default = "allow"` plus a static deny list of clearly-dangerous
ops — `sudo`/`dd`/`mkfs`/`launchctl`, writes to `/etc`/`/System`/`.ssh`/`.aws`/
`.smooth/auth`, …) and adopts it. So benign calls (read/list/grep/web_search/most
bash) run without prompting and only dangerous ops are blocked; narc (Gate 2)
remains the semantic backstop for context-dependent danger (`rm -rf`, `curl | sh`).

- The starter is a transparent, auditable, user-editable file — not a hidden
  in-code default. An existing `permissions.toml` is never overwritten, and a
  malformed one still fails safe to all-`ask`.
- If the file can't be persisted (read-only fs / perms), the daemon adopts the
  same starter posture in-memory rather than reverting to the old all-`ask`
  default (`crates/smooth-daemon/src/hooks/auto_mode.rs`).
