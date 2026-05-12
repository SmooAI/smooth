---
'@smooai/smooth': patch
---

TUI quality-of-life + agent context handling:

- `th --resume` / `th --list` / `th --agent` work at the top level (no
  need to type `th code --resume`). Pearl th-resume-top-level.
- Terminal resize no longer leaves duplicated tool-call rows above the
  inline viewport — `Event::Resize` clears the old viewport area before
  the next draw repaints. Pearl th-f294fd.
- Agents no longer reply "I don't have context about what 'that'
  refers to" when the user uses a pronoun pointing at the prior turn.
  Two fixes: prompt guidance in fixer/oracle that names the pronoun
  patterns and the recovery path, plus a runner-side sanitizer that
  replaces malformed `<function=…>` / `<tool_call>` pseudo-XML in
  prior history with a clear `[NOTE: …did NOT execute]` marker so
  the model reasons about its own past attempt instead of staring at
  unparseable XML. Pearls th-c366ff, th-c65ca3.
