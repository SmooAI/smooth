---
'@smooai/smooth': minor
---

SmoothFlow: the engine now advertises `replay` in `flow.hello.capabilities`
(th-8dbb42). A tmux session answers a `replay: true` attach or lag with an
empty `flow.replay` at the bridge's last `seq` and then has tmux redraw the
bridge's screen, so a second client joining a quiet session sees it at once.
The relay keeps the engine's `seq` on phone output, splits `flow.replay` into
≤16 KiB parts sent back to back after dropping the held output the replay
covers, and caps a phone's replay budget at 256 KiB. `th flow attach` applies
replays with the shared `ReplayOrder` logic.
