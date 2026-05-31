---
'@smooai/smooth': minor
---

smooth-operator: add an `LlmProvider` trait + `MockLlmClient` test harness (SMOODEV-1467).

`LlmProvider` abstracts the LLM call (`chat` + `chat_stream`); the real `LlmClient` implements it by delegating to its inherent methods. `MockLlmClient` is a deterministic, scriptable test double (text / tool-call / error / streaming-event responses) that records every request for assertions and is cheap to clone (shared state). This is Phase 0 of the LangGraph-parity work (epic SMOODEV-1466) — the seam every later phase (durable checkpointing, HITL pause/resume, persistent memory, vector RAG, structured output, OTel gen_ai spans) is unit-tested against. 10 unit tests + a doctest; clippy/fmt clean.
