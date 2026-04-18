---
"@smooai/smooth": minor
---

TUI autocomplete: `@` for file paths, `/` for slash commands.

The file-reference autocomplete state has always existed in
`smooth-code` but was never wired into the event loop or
rendered. Now it is, plus a parallel slash-command flow.

- **`@`** anywhere in the input box pops the file picker with every
  entry in the workspace file tree. Type to narrow by
  case-insensitive substring on filename.
- **`/`** at the start of the input pops the slash-command picker
  listing every registered command (`/help`, `/clear`, `/model`,
  `/save`, `/sessions`, `/quit`, `/status`, `/compact`, `/diff`,
  `/tree`, `/fork`, `/goto`, …) with one-line descriptions. Type to
  narrow by case-insensitive prefix.
- **Up/Down** arrows move the selection, **Tab** or **Enter**
  accepts, **Esc** closes the popup, typing a space ends the active
  query. Backspace past the trigger char closes it.
- Popup is a floating overlay anchored just above the input box, so
  the eye doesn't jump far from where you're typing. Orange border +
  "▶ " marker on the selected row to match the rest of the brand.
- New types: `CompletionKind { File, Command }`, detail line on
  `AutocompleteResult`, `trigger_pos` on `AutocompleteState`.
- New methods: `activate_commands`, `update_command_query`,
  plus two regression tests.
