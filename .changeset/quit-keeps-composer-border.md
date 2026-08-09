---
'@smooai/smooth': patch
---

Quitting th code no longer amputates the composer's bottom border. ratatui leaves the hardware cursor on the input row inside the box, and the teardown's single newline only reached the border row — so the next shell prompt overwrote it on every quit. Teardown now walks the exact number of rows from the cursor past the border (the same wrap arithmetic the renderer uses), emitting newlines so it also works when the box sits on the terminal's last row.
