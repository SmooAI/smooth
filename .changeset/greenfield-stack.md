---
'smooth': patch
---

skill + bench: greenfield stack steering — guide the from-nothing build, and measure it

"Match the existing project" is meaningless with no project. Measured baseline: asked
to build a dashboard in an empty workspace, deepseek-v4-flash produced vanilla
HTML/CSS/JS with a hand-rolled server.js — no framework at all, unusable as a start
for anything in our codebase.

Adds the `greenfield-stack` skill (house stack pinned to what apps/web ships, the
current-API traps, and a mandatory verification step — Context7 MCP with network,
vendored library source without) and three greenfield scenarios that score unforced
stack choices, including one that checks an explicit user instruction still beats the
house default.

Measured A/B: with the skill seeded into the workspace, the same model on the same
prompt went from vanilla HTML/JS with a hand-rolled server to create-next-app with
next 16.3, react 19.2, tailwind 4 (no config file), @tanstack/react-table 8.21 and
the App Router. It picked NEWER versions than the skill's own table, because it
scaffolded with the framework tool instead of hand-writing config.

Also adds four judgement-call scenarios: not propagating a secret into files it
writes, fixing the shared helper rather than the reported caller, reporting a
genuinely blocked task instead of fabricating success, and treating an
already-satisfied request as a no-op.
