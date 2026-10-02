---
'@smooai/smooth': minor
---

SmoothFlow Desktop renders terminals with libghostty-vt (th-872ea8). The Linux/Windows GPUI app drops `alacritty_terminal` and links libghostty-vt, built from the same pinned manaflow-ai/ghostty commit the Mac, iOS and Android apps use (`apps/smoothflow-desktop/ghostty-vt.lock`, `scripts/build-ghostty-vt.sh`). Styled runs, the Catppuccin 256/truecolor palette, the cursor, wide chars and the alternate screen come from Ghostty, and the terminal's DA/DSR replies go back to the session as `flow.input`.
