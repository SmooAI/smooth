---
'@smooai/smooth': patch
---

Stop now stops Big Smooth, and you can steer a running turn (th-74ba1f). The web and desktop clients sent `{action:'interrupt'}`, which the engine never supported, so Stop was always a no-op: the turn ran to completion and, on 2026-09-23, sent a second iMessage after Stop was pressed. The SPA also treated the engine's `UNSUPPORTED_ACTION` reply as the end of the turn, so the UI and the server disagreed about whether anything was running. The clients now send the engine's `cancel`, treat its `cancelled` event as terminal, only end a turn on events that carry that turn's `requestId`, and fall back to ending it locally after 10s if no reply comes. Phones already installed keep working over Smoo Relay because the daemon's relay bridge rewrites their `interrupt` to `cancel`. A message typed mid-turn now offers a choice. **Queue** sends it after the turn, as before. **Steer** (the button, ⌘/Ctrl+Enter, or "Steer now" on a queued chip) cancels the running turn, waits for it to actually end, and sends the message right away so it redirects the work.

iMessage no longer double-sends (th-646c22). A send that timed out or failed ambiguously used to report failure even when Messages had delivered it, so the model retried. Those sends are now checked against chat.db (read-only) and reported truthfully as sent, not sent, or unknown. An identical text to the same chat within 2 minutes is refused unless the call passes `allow_duplicate: true`.

The daemon now logs every tool call at INFO (th-5d48ca): name, call id, argument key names, duration and outcome. It never logs argument values or results.
