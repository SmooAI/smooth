---
'@smooai/smooth': patch
---

Add `smooth-bench convo` — an agentic conversation harness that catches quality regressions a single-turn bench can't see (pearl th-f19853).

An LLM driver plays a realistic user across several turns on ONE canonical-protocol session against a live Big Smooth; an LLM judge then grades the whole thread 1–5 on helpfulness, correctness, tool use, and consistency across turns, plus a rubric PASS/FAIL. Transcripts are emitted as JSON-lines.

Ships four scenarios: the "list my calendar events" ask that produced three contradictory answers, a rapid-correction/barge-in scenario that documents the th-3a912a interrupt gap (marked `expect_fail`, so it records XFAIL today and XPASS — loudly — once interrupts land), and two ordinary helpfulness/tool-correctness asks.

Also adds `CanonicalSession` to the bench's canonical driver: multi-turn conversations on one connection, with send and collect split so a message can be fired before the previous turn finishes. The suite is a `smooth-bench` subcommand, never part of `cargo test`.
