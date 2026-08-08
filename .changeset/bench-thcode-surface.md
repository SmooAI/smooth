---
'@smooai/smooth': patch
---

bench: `--surface {daemon,thcode}` — run the agentic corpus through `th code`'s client

`th code --headless` and the Big Smooth PWA speak the same canonical WebSocket to
the same daemon, so this is two client codepaths onto one engine rather than two
backends. `--surface thcode` drives turns through `smooth_code`'s own
`run_headless_capture` (not a re-implementation), so a difference between the two
surfaces is attributable to `th code`'s own layer — the cast role it requests and
the working directory it pins. The surface is recorded on the run and in every
JSONL record.
