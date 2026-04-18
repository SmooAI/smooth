---
"@smooai/smooth": minor
---

`th code --resume` and auto-generated session titles.

- **Session titles.** `Session` now carries an optional
  `title: Option<String>`. The TUI's input handler detects the first
  user message, spawns a detached `smooth-fast` call to generate a
  3–6 word Title Case summary, and stores it on `AppState`. Chat
  latency isn't gated on the name. Previously saved sessions without
  titles still load — `SessionSummary::display_label()` falls back
  to the message preview.
- **`th code --resume [query]`**. New CLI flag. Resolution tiers:
  exact id → unique id prefix → unique title substring
  (case-insensitive) → unique preview substring. No argument picks
  the most recently updated session. Ambiguous matches error with
  the candidate list. Reuses the same auto-naming pipeline as the
  web chat so titles are consistent across TUI + web.
- **`th code --list`**. Prints saved sessions newest first with
  display label, short id, and updated time, then exits without
  launching the TUI.
- `AppState::from_resumed_session()` + `app::run_with_session()`
  restore a persisted session as the starting state. The welcome
  message is suppressed on resume in favor of a "Resumed session: &lt;title&gt;"
  marker.
- Six regression tests on `SessionManager::find_by_query` +
  `most_recent` covering each tier and the ambiguous-match path.
