---
"@smooai/smooth": patch
---

SmoothFlow macOS: the ghostty clipboard callbacks no longer `assumeIsolated` on the main actor unconditionally. Ghostty's renderer/IO threads can invoke runtime callbacks (the source of the 2026-09-08 SIGTRAP crash report); they now hop to main when off it, matching the action callback fix in #527.
