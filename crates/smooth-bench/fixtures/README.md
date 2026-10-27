# smooth-bench fixtures

Small, checked-in repos for the change-existing-code suite
(`../code-change-scenarios.toml`, SMOODEV-4125). Each `<name>/` is seeded
into the agent's workspace before the turn; each `<name>.holdout/` is
copied in AFTER the turn, overwriting the agent's files at the same paths,
and then `npm test` runs. The agent never sees the holdout tests and
cannot weaken them.

Zero dependencies on purpose: `node --test` only, so the checks need no
`npm install` and no network.

These are excluded from `oxfmt` (`.oxfmtrc.json`): `order-report` is
deliberately messy, because tidying it is the task.
