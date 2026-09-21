---
'@smooai/smooth': patch
---

SmoothFlow 0.2.4

- **Detaching no longer logs out your shell.** `portable-pty` typed a newline and `^D` into the pane whenever a terminal bridge was dropped, so closing the last window on a session — or a second client attaching, like the phone alongside the Mac — could log out a shell at its prompt or hand an agent an EOF. Two overlapping attaches also no longer evict each other's connection (#614).
- **Seven more agent CLIs, with real lifecycle hooks:** Gemini CLI, Qwen Code, Cursor Agent, Droid, Copilot, Amp and Pi. Permission requests from Gemini and Droid can now be approved (#600).
- **`th harness doctor`** reports which harnesses actually work on this machine, plus a conformance contract every harness manifest must pass (#598).
- A zombie process is no longer mistaken for a live agent after Kill, and Amp no longer waits 90s before its prompt is pasted.
- Engine end-to-end suite (#562) and the App Store reviewer demo mode (#577).
