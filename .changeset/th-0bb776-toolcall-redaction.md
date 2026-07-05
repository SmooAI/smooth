---
'@smooai/smooth': patch
---

Redact secrets from chat `tool_calls` before they are persisted to the Dolt session store (pearl th-0bb776). Since pearl th-880f2c, `session_messages` stored tool-call `arguments` and `output` JSON verbatim, forever, with zero redaction — three leak classes: (1) a user pasting `curl -H "Authorization: Bearer sk-..."` that the agent runs, (2) `env`/`printenv`/`git config --list` output captured as a tool result, and (3) `read_file` paths like `~/.aws/credentials`.

`DoltSessionStore::save_message` now runs Narc's `SecretDetector::scan` over each tool call's arguments and output and replaces every match with a `[REDACTED:<type>]` marker before the INSERT. A new `redaction_applied: bool` (serde-default `false`, so pre-existing rows still deserialize) rides along on `SessionToolCall` so downstream consumers know a record was scrubbed, and an audit-log entry (`toolcall_redaction`) fires whenever redaction happens. The live in-memory conversation is untouched — only what is written to durable storage is scrubbed. Clean tool calls are left byte-identical with the flag `false`.
