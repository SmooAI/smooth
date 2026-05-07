---
"@smooai/smooth": minor
---

Cleanup batch: empty-arg normalization, oracle prompt tightening, real args/result on tool-call events

Three pearls bundled (`th-75c3e5`, `th-962395`, `th-7a5106`):

**Empty-args normalization (`th-75c3e5`)**
Some small models (Gemini Flash family especially) emit a literal
`""` empty-string for tools that take no parameters, instead of
the schema-correct `{}`. Downstream hooks + tools that expect an
object then fail on what should have been a no-op call. Fix in
`smooth-operator::ToolRegistry::execute` + `execute_single`:
normalize `Value::String("")` and `Value::Null` args to `{}`
before any hook runs. `project_inspect("")` now succeeds.

**Oracle prompt: don't bail after one tool error (`th-962395`)**
Symptom: oracle would call `project_inspect`, get a tool error,
then declare "I'm unable to list files as well" without ever
calling `list_files` (which is in its allowlist). Prompt now has
a "When a tool errors, try a different one" section that
explicitly says: a single error doesn't mean the tool is
unavailable; the system-prompt allowlist is the truth; pivot to
the next sensible tool. Lists concrete fallbacks for the common
cases (`project_inspect` → `list_files` + marker reads, `read_file`
404 → next likely path, `grep` empty → broaden / `glob`).

**Real arguments + result on tool-call events (`th-7a5106`)**
`AgentEvent::ToolCallStart` only had `iteration` + `tool_name`;
`ToolCallComplete` had `iteration` + `tool_name` + `is_error`.
The full args / result / duration only flowed via the separate
`ReporterEvent` HTTP channel — sandboxed dispatch, which parses
the runner's stdout JSON-lines, ended up forwarding `arguments:
String::new()` and `result: String::new()` for every inner tool
call. So inner `read_file` / `list_files` / `grep` calls
rendered with empty args (or, in the user's experience, didn't
render at all because the empty preview made them indistinguishable
from each other).

Adds:
- `AgentEvent::ToolCallStart::arguments: String` (default `""`
  for backward-compat).
- `AgentEvent::ToolCallComplete::result: String` + `duration_ms:
  u64` (default `""` and `0`).
- All emit sites populate the new fields.
- Big Smooth's stdout parser (`server.rs`) reads them and forwards
  in `ServerEvent` instead of empty strings.
- The TUI's `run_agent_streaming` already uses these fields — they
  just have real values now, so inner `read_file` / `list_files` /
  `grep` calls render inline with proper args + duration + result
  preview.
