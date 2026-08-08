---
'@smooai/smooth': patch
---

th code's idle screen now shows who you're talking to and with what. The status bar read `fixer · unknown` until the first turn because nothing asked the daemon which model it would run — a new ungated `GET /api/mode` answers from the same resolution the engine uses (env → providers.json, model name only, never credentials), and th code fetches it at startup into the display-only model label; anything the user chose via `--model` or the picker still wins. The splash subtitle now names Big Smooth instead of reciting the platform tagline, and a hint line surfaces the daemon-backed powers the th-d7366d epic landed (`@` file/pearl mentions, Ctrl+B conversations, image paste, `/skill` catalog) — none of which were visible anywhere at idle.
