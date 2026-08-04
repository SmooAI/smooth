---
'@smooai/smooth': patch
---

`th` no longer emits ANSI escapes when stdout is not a terminal.

owo-colors styles unconditionally — `.cyan()` has no tty check and
`set_override` only gates `if_supports_color` — so every `th` printer leaked
escape sequences into pipes, `$(…)` capture, and agent hooks. `NO_COLOR=1` did
not help. `th pearls prime` running under a Claude Code SessionStart hook was
the worst case: a whole context block of escape soup on every session start.

All printing now goes through `anstream`, which strips escapes when stdout is
redirected and honors `NO_COLOR` / `CLICOLOR_FORCE`. Real terminals are
unchanged — still fully colored.
