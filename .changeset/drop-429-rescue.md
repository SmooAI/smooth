---
"@smooai/smooth": minor
---

Remove the 429 auto-retry rescue from `th claude` (th-2d5c45).

`th claude` was built when a transient HTTP 429 ("Server is temporarily limiting
requests · Rate limited") would strand a supervised Claude Code session, so the
supervisor detected the throttle, backed off with jitter, and resent the last
message until it landed. **Latest Claude Code retries that throttle internally**,
making the rescue dead weight — and worse, it could double-send a prompt on top
of a model that was already recovering.

**User-visible behaviour removed**: the supervisor no longer backs off or resends
anything. A transient throttle is now simply watched, like any other pane state.

- Deleted `crates/smooth-cli/src/claude/governor.rs` (the whole `RateLimitGovernor`
  + backoff/jitter module) and `PaneState::RateLimited` /
  `is_retryable_rate_limit()` / the throttle marker list in `detect.rs`.
- Deleted `extract_last_user_message()` (and its `gutter_content` helper) — the
  resend path was its only caller.
- Dropped `Mode::rescues()`; as a consequence `manual` and `paused` now differ
  from `driving` only in whether the supervisor sends the initial prompt.

**Unchanged**: `PaneState::UsageLimit` still stops the supervisor — the 5-hour and
weekly account caps are a different mechanism and were never auto-retried. The
`th claude run` / `ls` / `attach` / `mode` / `tui` surface, the tmux supervisor
loop, and behaviour on every non-throttle state are all the same.
