---
'@smooai/smooth': patch
---

SmoothFlow (macOS): terminals resize after a display change. Undocking, plugging in a monitor or changing a resolution now re-binds the renderer to the new display (ghostty_surface_set_display_id, which the app never set), re-applies scale and size, and reports the new grid on Ghostty's cell-size change (th-b9e6df).
