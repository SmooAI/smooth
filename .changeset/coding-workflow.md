---
"smooai-smooth-operator": minor
"smooai-smooth-operator-runner": minor
"smooai-smooth-bigsmooth": minor
"smooai-smooth-code": minor
---

**CodingWorkflow** — first real per-phase dispatcher. ASSESS / PLAN / EXECUTE / VERIFY / REVIEW / FINALIZE each run their own `Agent` invocation through a different `Activity` slot: Thinking for ASSESS + FINALIZE, Planning for PLAN, Coding for EXECUTE + VERIFY, Reviewing for REVIEW. Previously Thinking / Planning / Coding / Reviewing were declared-only — no code path routed through them.

ASSESS now emits a structured `## Goal Summary` section that's threaded through every later phase's user prompt so the agent stays anchored to the objective across review loops. REVIEW can refine the goal summary via an `## Updated Goal Summary` block when it realizes the understanding drifted. FINALIZE checks the final state against the Goal Summary, not just test results.

Opt-in via `SMOOTH_WORKFLOW=1` in Big Smooth's environment. When set, Big Smooth serializes the `ProviderRegistry` via `ProviderRegistry::to_json` / `from_json` (new) and passes it to the sandboxed runner in `SMOOTH_ROUTING_JSON`. The runner deserializes and dispatches the workflow; otherwise falls back to the existing single-Agent loop.

`AgentEvent::PhaseStart { phase, alias, upstream, iteration }` emitted at each node entry. TUI listens, tracks `current_phase` / `phrase_idx` in `AppState`, and renders the phase prefix + rotating thesaurus phrase in the status bar:

```
ASSESS · smooth-thinking → kimi-k2-thinking | Pondering… | tokens: 1.2k | spend: $0.003
```

`smooth_code::thesaurus` provides the rotating phrase lists (Pondering… / Hammering… / Nitpicking… per phase). Spinner ticks advance the cycle.

Companion fixes: `BoardroomNarc` now routes through `Activity::Judge` instead of the Default slot (what the Judge alias was named for), and `ToolRegistry` is `Clone` so multiple phase Agents can share the same tool handles.
