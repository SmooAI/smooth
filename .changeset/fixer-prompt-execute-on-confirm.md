---
"@smooai/smooth": patch
---

fixer prompt: add explicit "When the user confirms: EXECUTE" rule

When the prior assistant turn enumerated a destructive plan ending in
"Proceed?" and the user's next message is "yes" / "proceed" / "go" /
"do it" etc., the agent must invoke the destructive command directly,
not re-enumerate or re-ask for confirmation, and not pivot to a
different task.

Lifts `cleanup-node-modules-orphans` pass rate from 0/5 to 3/5 under
strict-coach mode (minimal "yes, proceed" reply). The old prompt
implied the meaning of "yes" but never explicitly told the agent what
behavior to perform on receipt — the model was free to interpret
"yes" as a context-restate cue, which the bench's idle detector then
mistook for a fresh first-idle and pasted the coach reply again,
producing the score-0.55 zero-bytes-freed failure shape.

Pearl: th-e182bc (re-scoped — was misdiagnosed as inter-turn context
loss; instrumentation confirmed prior_messages flow is intact through
all 3 hops, the failure is in agent action policy)
