---
"@smooai/smooth": minor
---

TUI: redesign `/model` as an activity-slot picker. Top level lists the 8 routing slots (Thinking / Coding / Planning / Reviewing / Judge / Summarize / Default / Fast) with their current model. Enter on a slot opens a sub-picker of candidate models; selecting one applies the routing and persists it to `~/.smooth/providers.json`. Up/Down navigates, Esc backs out (Models → Slots → closed) — previously the picker had no input handling at all and Esc didn't dismiss it.
