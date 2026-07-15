---
"@smooai/smooth": minor
---

Big Smooth gains an intelligent `th` tool + re-homed auto-mode & narc-judge safety
hooks on the smooth-operator engine (th-3119e3 / th-1f694a / th-515a13).

- **Engine**: bumped to core `0.16.2` (crates.io) + `smooth-operator-server`/`svc`
  @ the new `.tool_hooks(…)` builder seam on `LocalServer` (th-1f694a upstream).
- **Intelligent `th` tool** (`smooth-tools`): a native agent tool that teaches Big
  Smooth its own CLI surface — web search, knowledge retrieval, crawl, `th api …`,
  pearls — so it uses `th` deliberately instead of blind bash. Resolves the `th`
  binary, runs it with structured argv (no shell interpolation), caps output.
- **auto-mode `ToolHook`** (`smooth-daemon`): permission gate via the in-tree
  `smooth_policy::auto_mode` rule engine (allow/deny/ask). `SMOOTH_AUTO_MODE`
  selects posture; the daemon defaults to `bypass` (usable out of the box, narc
  still guards) until the interactive approval queue lands (th-1f7fd7).
- **narc `ToolHook`** (`smooth-daemon`): tool-call surveillance — secret/dangerous-
  CLI/prompt-injection detectors (recovered from the old smooth-narc crate) with an
  LLM-judge escalation via the daemon gateway (FAST_MODEL), fail-closed on
  block/timeout, and `post_call` secret redaction through the `&mut ToolResult`
  seam. Degrades to regex-only + redaction when no gateway key is configured.
- Both hooks install via `.tool_hooks([auto_mode, narc])` — auto-mode first
  (permission), narc second (surveillance) — gating every tool call, including
  SEP extension tools.
