---
'@smooai/smooth': patch
---

smooth-bench: rewire the live-drive path onto the canonical smooth-operator
`LocalServer` WebSocket protocol so the engine-parity sweep actually scores a
real turn.

The old default driver (`chat_driver`) assumed the deleted microVM "create a
pearl + dispatch a teammate" model, and the legacy `SMOOTH_BENCH_LEGACY_DIRECT`
path spoke the retired bespoke `/ws` handshake (waited for a `Connected` event
the engines never send → "Timed out waiting for Connected"). Both are gone.

- **New `canonical_driver`** speaks the schema-driven protocol every engine
  now uses (`create_conversation_session` → `immediate_response.sessionId` →
  `send_message` → drain to `eventual_response`), modeled on the daemon's
  `OperatorTurnDriver::drive_once`. It parses `stream_chunk` tool-result events
  into the `BenchResult`'s tool-call records and does a best-effort cost scan
  (the polyglot servers don't surface cost → `$0`, noted). Fully unit-tested
  (message construction + event classification, no live server needed).
- **Workspace-per-task boot**: the engine is now booted *per task* with its
  workspace pointed at that task's scratch `work_dir` — rust via
  `SMOOTH_WORKSPACE`; go/ts/python/dotnet (no workspace env) with
  `cwd = work_dir`. Go is launched from a prebuilt binary (`go run` can't
  launch from a foreign cwd), the rest via an absolute path / `--project`.
- Retired `chat_driver` + the `smooth-code` headless dependency.

Verified live: the go engine boots per task, the canonical turn runs, the test
suite executes, and a real scored table is produced (nonzero wall-times). The
rust daemon engine additionally exercises real file edits in the task workspace
(`write_file` tool result parsed, file lands in `SMOOTH_WORKSPACE`).

Known follow-up (out of scope for the driver rewire): the polyglot `cmd/serve`
binaries call `ServeLocal` **without `WithTools(...)`**, so their agents have no
file/bash tools — they can't edit the solution. Only the rust daemon ships a
coding toolset today. Giving the polyglot engines a coding toolset (via
`WithTools` or a bench-supplied extension) is a separate smooth-operator change.
