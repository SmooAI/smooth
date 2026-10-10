---
'@smooai/smooth': patch
---

SmoothFlow for Mac and SmoothFlow Desktop now consume `flow.replay` (th-7e46cd): an attach asks for a replay when the engine advertises one, output waits for it, each complete replay starts the terminal over and output it covers is dropped, and a broken chunked replay re-attaches. Both clients stop answering DA/DSR queries for sessions on the engine-owned PTY host, which answers them itself. The mock flow server serves the replay fixture, with tests (`pnpm test:mock`).
