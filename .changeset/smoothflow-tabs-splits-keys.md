---
'@smooai/smooth': patch
---

SmoothFlow: tabs, directional splits, and a keyboard config pane (th-27baa4)

The surface area is now a stack of tabs, each holding a binary split tree
instead of a flat row of panes — so ⌘D splits right, ⌘⇧D splits down, ⌘⇧←/⌘⇧↑
split the other two ways, ⌘⌥arrows move focus between panes by geometry, ⌘⇧↩
zooms without disturbing the layout, ⌘⌥= equalizes, and ⌘T/⌘W/⌘⇧[/⌘⇧] work the
tabs. ⌘⇧T opens a shell session in the focused session's worktree in a new tab.

Every shortcut is user-configurable. Settings ▸ Keyboard lists every action with
its binding, records a new one, flags conflicts and resets per-row or all; it
writes ~/.smooth/smoothflow/keybindings.toml, which you can also edit by hand.
Only overrides are stored, a bad line loses that line rather than the map, and
the menu bar is built from the keymap so a rebind moves the menu with it.

Two defaults changed (both rebindable): Steer All Working is ⌘⌥↩, freeing ⌘⇧↩
for Zoom Pane as in every other terminal; the old untyped "Split Surface" is now
Split Right. See docs/Engineering/SmoothFlow-Keybindings.md.
