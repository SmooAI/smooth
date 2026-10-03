---
'@smooai/smooth': minor
---

New `smooth-flow-vt` crate (th-5025fb, ADR-011 phase 2): the SmoothFlow session host's headless libghostty-vt terminal. It feeds pty output, reads the visible screen as text (what `tmux capture-pane` gave), reports `alternate_on`, the cursor, the OSC title, bracketed paste and DECCKM, and encodes manifest key names and pastes for the program's current input modes. It also produces the bounded VT replay snapshot a client applies on attach. While a TUI holds the alternate screen, that snapshot keeps the primary screen's history, and it puts back the blank bottom rows that libghostty-vt's formatter leaves out. The libghostty-vt build script and pin moved from `apps/smoothflow-desktop/` to `scripts/ghostty-vt/`, shared by the desktop app and the new crate. `smooth-flow` takes the crate only behind the off-by-default `pty-host` feature. PR CI and the release job now build libghostty-vt (cached on the lock and the script).
