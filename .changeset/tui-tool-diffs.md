---
"@smooai/smooth": minor
---

th smooth TUI: render unified diffs for edit_file / write_file / apply_patch

Tool-call rendering used to show only `tool_name("...args preview...")
── done`, with the actual change buried in a collapsed-by-default
output blob. Worse, tool calls weren't even being attached to the
assistant message in the first place — `ServerEvent::ToolCallStart`
and `ToolCallComplete` translated into stub `AgentEvent`s that the
event handler dropped on the floor (`_ => {}`). So the chat showed a
wall of streaming text, no separate tool-call indicators, and zero
information about what was edited.

Two changes:

- **Plumb tool calls through to state.** `run_agent_streaming` now
  takes the AppState `Arc<Mutex<_>>` and mutates it directly when
  it sees `ServerEvent::ToolCallStart` / `ToolCallComplete`. Tool
  calls hang off the most recent assistant message; ordering is
  preserved per-tool-name via a small per-name pending queue
  (`HashMap<String, VecDeque>`). The streaming assistant message
  is created synchronously before the recv loop so fast-arriving
  tool starts have somewhere to land.
- **`crate::tool_diff` module.** `pub fn render(tool_name, args)`
  returns `Option<Vec<Line<'static>>>`. Recognizes `edit_file`
  (uses `path` + `old_string` + `new_string`), `write_file`
  (renders the new content as all-`+`), and `apply_patch`
  (renders the provided patch verbatim with consistent styling).
  Uses the `similar` crate for unified-diff generation with a
  2-line context radius. Caps at 200 rendered lines per call —
  big diffs get an `… N more diff lines elided …` marker in the
  middle. 7 unit tests.
- **`inline::message_lines`** now suppresses the noisy
  `("...args preview...")` payload + the collapse glyph on
  diff-rendered tool calls (the diff itself is the content), and
  appends the styled diff lines after the header.
- **`ToolCallState::arguments_full: Option<Value>`** preserves
  the parsed arguments for the renderer to consume. Marked
  `#[serde(skip)]` so saved sessions don't bloat with full file
  contents on every edit. New `ToolCallState::from_raw(id, name,
  arguments_json: &str)` constructor for the WS dispatch path.
- **`AppState::start_streaming` is now idempotent** — eager
  synchronous call (in `run_agent_streaming`) plus the lazy call
  (in `handle_agent_event`) no longer produce a duplicate empty
  assistant message.
