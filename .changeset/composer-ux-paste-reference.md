---
'@smooai/smooth': patch
---

th code's composer catches up to Claude Code ergonomics. A large text paste (over 5 lines or 400 chars) no longer floods the draft — it stages as a compact `[Pasted #N — X lines]` reference that expands back into the real text at send, and deleting the reference drops the paste. The input box's growth ceiling is now responsive: the inline viewport scales to 40% of tall terminals (fixed 14 rows before) and the composer grows up to 16 text rows there, while short terminals behave exactly as before. Word and line editing landed too: Alt+Backspace kills the previous word, Cmd+Backspace kills to line start, and Ctrl+W / Ctrl+U are the always-works spellings for terminals that never deliver those modifiers.
