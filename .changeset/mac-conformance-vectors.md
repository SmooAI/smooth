---
'@smooai/smooth': patch
---

SmoothFlow for Mac replays the Client Spec conformance vectors (th-3e6020), and it fixes directional focus. In a layout with a tall pane beside a stack, moving focus down from the top of the stack jumped sideways to the tall pane. Now only a pane beyond the focused pane's edge counts, and an exact tie always goes to the same pane instead of whichever one the dictionary happened to list first.
