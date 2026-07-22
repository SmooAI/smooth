---
'@smooai/smooth': patch
---

smooth-agent: auto-onboard every Claude Code session to the th-mail bus +
agentic self-rename (`th agent rename`).

Previously only `th claude run` workers registered on the bus. Now `register-agent.sh`
(SessionStart) registers **every** session — a plain `claude` gets a placeholder
handle (`cc-<cwd-basename>-<sid4>`) registered with `--no-push` (cheap on every
start), and a new `on-first-prompt.sh` (UserPromptSubmit) fires once to nudge the
session to rename itself to a task-meaningful handle via
`th agent rename --from <placeholder> --to <new>` (which carries its mail over).
Workers keep their meaningful `SMOOTH_AGENT_HANDLE` and are never nudged.
Registration is always-on and safe; no background `th msg watch` is auto-started
(Dolt is single-writer — many watchers cause "database is read only"), so
push-watching stays opt-in via `/th-mail`.
