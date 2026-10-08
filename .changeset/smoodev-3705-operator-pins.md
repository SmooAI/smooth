---
'@smooai/smooth': minor
---

Big Smooth: bump the agent engine to core `1.14.2` and the smooth-operator server to `19da0e9`. Turns now survive a client disconnect, image-only sends work, history replays images, failed turns are recorded in the conversation, and a conversation runs one turn at a time. Core now stops a hung tool call after 120s, so long-running tools set their own limits: `bash` follows its `timeout` argument, `flow_prompt_wait` its own wait, `th` gets 30 minutes, plugins 10, and session setup and `add_harness` 15. Sidekick tools no longer count a permission prompt against their limit. The web app shows a "turn still running" banner with a Stop button when a send is refused with `TURN_IN_PROGRESS`, and shows recorded turn failures as error cards.
