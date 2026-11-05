---
'@smooai/smooth': minor
---

SMOODEV-4127: ⌘N in SmoothFlow now shows a prompt in about 50–90 ms instead of about 0.7 s.

- **Pre-warmed shells.** The engine keeps a spare shell already at a prompt (up to three, one per directory, in hidden `flowspare-*` tmux sessions), and a new shell in that directory claims it. The app hints where the next ⌘N will open with the new `flow.prewarm` frame. `SMOOTH_FLOW_PREWARM=0` turns this off.
- **Faster shell creation.** Creating a shell no longer runs pearl inference on the request path. That work now happens in the background.
- **Live tab names.** Shell tabs are named like cmux names them: by the running program (its OSC title, else its command name), else by the folder.
- **Rename Tab (⌘⇧R).** Rename Tab opens an inline popover: Esc cancels, and an empty name goes back to the automatic one. You can rebind it in `keybindings.toml`. The name is pinned on the session row (`flow.rename`, `POST /api/flow/sessions/{id}/rename`, `th flow rename`, additive `title_pinned`), so the phones and `th flow ls` show it as well.
