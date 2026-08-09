---
'@smooai/smooth': patch
---

bench: wait for the port to free instead of failing fast

The "refuse to attach to a process we did not spawn" check was correct for concurrent
runs and wrong within a single one: the agentic suite boots a fresh engine per
scenario, and the previous engine's socket is still closing when the next one starts.
It turned 23 of 28 scenarios into "engine boot failed" on its first real run.

Now polls for up to 20s. A few seconds of patience distinguishes "the last scenario is
still letting go" from "someone else owns this", and the refusal still fires for a
genuine collision.
