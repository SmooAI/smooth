---
"@smooai/smooth": patch
---

th smooth TUI: restore the gradient SMOOTH wordmark welcome banner

The previous pearl (Viewport::Inline switch) marked the welcome
banner as `#[allow(dead_code)]` because there was no longer an
empty-state region inside the viewport to paint it in. Bring it
back the inline-native way:

- `render::welcome_banner_lines()` is the public builder — returns
  `Vec<Line<'static>>` for the gradient box-drawing wordmark + the
  "AI Agent Orchestration Platform" / "smoo.ai" / "type a message"
  tagline lines.
- `app::run` calls `inline::insert_before_lines` once at session
  start (fresh sessions only — resumed sessions skip the banner)
  to push it into the terminal's scrollback BEFORE any chat
  messages. It sits at the top of the session like a real
  terminal program's startup banner, scrollable, selectable,
  copyable like any other terminal output.
- The verbose "Welcome to Smooth. Type a message and press
  Enter to chat." system line is replaced by the shorter
  "Type a message to get started. /help for commands." (the
  banner already says the equivalent).
