---
'@smooai/smooth': minor
---

SMOODEV-4111: SmoothFlow starts like a terminal. ⌘N opens a plain shell in a new tab at once (no sheet), in the focused pane's live directory, else the last directory you opened, else `$HOME`. ⌘⇧ + an arrow splits the focused pane that way with a new shell in that pane's current directory (⌘D / ⌘⇧D stay as aliases), beating Ghostty's own ⌘⇧↑/↓. The agent dialog is opt-in: ⌃⌘N, or the sidebar's new + split button, which also offers New Shell in Directory… and Fan Out…. The engine's `flow.new` gains an additive `cwd_of` that starts a session in another session's live cwd (tmux `#{pane_current_path}`, falling back to its worktree).
