---
'@smooai/smooth': patch
---

SmoothFlow terminal font and glyphs (th-bcd819). The 0.2.1 blank starship prompt (`_` for `❯`, no branch/cloud icons) was tmux: a Finder-launched app has no `LANG`, so the child daemon's `tmux attach` client was treated as non-UTF-8 and every non-ASCII cell became `_`. The flow engine now passes `-u` to every tmux client and the app gives the child a UTF-8 `LANG`/`LC_CTYPE`. On top: JetBrainsMono Nerd Font ships in the bundle (OFL), registered per process and named in the ghostty overrides with a 13 pt default; Settings ▸ Terminal picks family / size / ligatures live; a `font-family` / `font-size` in the user's own Ghostty config still wins over the bundled default, a Settings choice over both; the app's monospace text uses the same face. `theme = …` in the user's Ghostty config now applies too: the bundle carries libghostty's themes and exports `GHOSTTY_RESOURCES_DIR`.
