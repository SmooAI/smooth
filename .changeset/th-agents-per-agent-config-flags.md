---
'@smooai/smooth': minor
---

`th api agents` grows typed flags for the per-agent config fields that went live server-side with SMOODEV-590 (per-agent behavior on all five polyglot smooth-operator servers).

`th api agents update <id>` now takes either a raw JSON patch body (unchanged) or typed field flags: `--instructions` (`instructions.prompt`, `@file` supported), `--greeting`, `--personality` (a preset name like `witty`, or a full `PersonalityConfig` JSON object / `@file`), `--visibility public|internal`, `--workflow` (`ConversationWorkflow` JSON `{goal, steps}` / `@file`), and `--tool-config` (`AgentToolConfig` JSON `{enabledTools}` / `@file`). Passing both a body and flags fails loudly, as does an update with neither. JSON flags are validated to be JSON objects client-side so a stray array/string fails with a clear message instead of a backend 400.

`th api agents mint` accepts the same `--personality` / `--workflow` / `--tool-config` at create time, alongside the existing `--instructions` / `--greeting` / `--visibility`.

Reading is unchanged: `th api agents show <id>` returns the full record including all six fields.
