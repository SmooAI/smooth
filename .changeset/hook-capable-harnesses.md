---
'@smooai/smooth': minor
---

SmoothFlow gains seven hook-capable harnesses: Gemini CLI, Qwen Code, Cursor Agent, GitHub Copilot CLI, Factory Droid, Amp and Pi (th-b00115). Each ships a built-in manifest and a `th pkg` overlay (hooks, Amp plugin or Pi extension) that reports its real lifecycle to the flow engine, so their sessions show working, idle and needs-you from the CLI itself instead of pane scraping. `th pkg install --harness` accepts gemini, qwen, droid, copilot, amp and pi as hook-only targets, and Cursor gets its hooks merged too.

Engine: agent panes carry `SMOOTH_FLOW_ID` so a CLI that cannot pre-assign a session id binds to its row. Only Claude-protocol harnesses hold a permission request open; other asks get an id that `th flow approve` answers by keystroke. Steering honors `steer.submit_key` and the new `steer.submit_delay_ms` (Gemini drops an Enter sent right after a paste). Proven live against Pi, Qwen and Gemini.
