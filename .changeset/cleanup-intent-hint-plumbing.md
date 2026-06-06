---
"smooai-smooth": minor
---

coding_workflow: cleanup-intent hint plumbing for continuation turns

The fixer's test-fix bias + cross-fixture pattern confabulation made
`cleanup-node-modules-orphans` chronically unreliable on v4-pro
(1/6 perfect in pane-captured samples — agents fabricating
`packages/db/db.test.js` on cleanup tasks; running
`find . -type f -size +150k -delete` on a node-modules orphan
task). The existing `is_cleanup_intent(task)` preamble in
`build_user_prompt` suppresses both failure modes — but it only
fires when the CURRENT user message matches cleanup verbs/nouns,
which the bench's "yes, proceed" coach reply does not.

This change plumbs a `cleanup_intent_hint: bool` through
`CodingWorkflowConfig`. The runner sets it by scanning
`agent_config.prior_messages` for cleanup intent — so when the
prior turn was a cleanup README, the workflow re-applies the
preamble on the confirmation turn via a new `is_confirmation_reply`
helper.

Net result at deepseek-v4-pro:

- `cleanup-node-modules-orphans`: prior 1/6 perfect (3/5 + 1 no-action
  + 1 catastrophic 7.2MB protected-dir delete) → **5/5 perfect,
  zero-variance identical 3,559,394 bytes**. Matches opencode's
  3/3 identical-bytes baseline on the same fixture.
- `cleanup-disk-bloat`: 3/3 → ~2/3 (~67% pass rate; one cross-fixture
  hallucination remained). Net regression on this fixture.
- `cleanup-impossible-task`: 3/3 → variance not yet characterized,
  early sample 1/2.
- `cleanup-pycache-debris`: 3/3 strong → 2/2 stable.

Trade-off worth shipping: eliminating the chronic
catastrophic-delete failure mode on node-modules (a fixture where
v4-pro previously had a 17% catastrophic + 50% no-action rate)
outweighs the marginal disk-bloat slip. Pearl th-e182bc.
