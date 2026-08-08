---
'@smooai/smooth': patch
---

bench: export SMOOTH_WORKSPACE for every engine, and refuse to attach to a foreign process

Two more cases of the bench measuring the wrong thing. Every polyglot host reads
`SMOOTH_WORKSPACE` and falls back to cwd, but the bench exported it only for Rust.
Go/TS/Python happened to work because their launchers run with cwd = workspace, while
`dotnet run --project` runs the app from the project directory — so .NET confined its
coding tools to the engine checkout, read files fine, and never wrote where the
scenario asserted. With it exported, .NET goes 0% → 100% and all five engines are green.

Ports are fixed per engine and `wait_for_port` only waits for *something* to accept TCP,
so a concurrent run or a leftover daemon would be silently attached to and scored as if
it were the engine we spawned. The bench now refuses to start on an occupied port and
explains why.
