---
"@smooai/smooth": minor
---

th smooth TUI: scroll, selection, markdown, and intent-aware dispatch

Four fixes to the chat TUI. They all stemmed from the same session
where "how do I run dev mode" caused the agent to write
`DEV_MODE_GUIDE.md` files and report a fabricated `1 passed, 0 failed`.

- **Drop `EnableMouseCapture`.** Mouse capture was on but the event
  loop had no `Event::Mouse` arm — wheel scroll was dead AND text
  selection was dead because capture stole the drag. The TUI doesn't
  consume mouse events, so dropping capture lets the terminal handle
  both natively.
- **Render assistant messages as markdown.** A new
  `smooth-code::markdown` module walks `pulldown-cmark` events into
  styled ratatui `Line`s. Bold, italic, inline code, fenced code
  blocks, headings, lists, blockquotes. Streaming-friendly: an
  unterminated fence renders as in-progress code rather than as raw
  backticks.
- **`/agent` and `/ask` commands.** `/ask` switches to the read-only
  `oracle` role for Q&A — denies `edit_file`/`write_file`/`bash` so
  the agent answers without modifying the workspace. `/agent <name>`
  switches to any built-in role. Both pin the role, disabling the
  intent classifier below.
- **Intent-aware dispatch.** When the user hasn't pinned a role, every
  message routes through a new `intent_classifier` shadow role (Fast
  slot, Haiku-class) that emits `WORK` or `QUESTION`. Questions
  dispatch under `oracle`; work dispatches under `fixer`. A pattern
  fallback keeps dispatch alive when the LLM gateway is unreachable.
- **Runner: gate coding workflow on the role.** The coding workflow
  forces a "run tests, iterate until green, report N passed/failed"
  loop. Running it under a non-Coding-slot role (oracle, mapper,
  heckler) was producing the hallucinated `1 passed, 0 failed` line.
  The workflow now only runs when `active_role.slot == Coding` AND
  `bash` is allowed.
