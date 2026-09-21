# ghostty resources

Copied into the bundle as `Contents/Resources/ghostty` (a folder reference in
`project.yml`) and handed to libghostty through `GHOSTTY_RESOURCES_DIR` at
launch (`GhosttyRuntime`), so `theme = …` in a user's `~/.config/ghostty/config`
resolves inside SmoothFlow exactly as it does in Ghostty.app (th-bcd819).

`themes/` is Ghostty 1.3.1's set — the `ghostty/` output of
[iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes) (MIT),
taken verbatim from `Ghostty.app/Contents/Resources/ghostty/themes`. Refresh
it from a newer Ghostty.app when the GhosttyKit pin moves.
