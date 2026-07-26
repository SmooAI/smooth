---
'@smooai/smooth': patch
---

Big Smooth: extend the system prompt to an industry-best-practice agentic
prompt (pearl th-67b1e1).

`BIG_SMOOTH_PERSONA` grew from ~70 words (identity + personal-not-support +
no-chain-of-thought) to a full sectioned agent prompt: **Agency** (finish
the job, adapt on failure, prefer doing over describing), **Tools** (use
them not guesses, never fabricate results, verify, parallelize),
**Memory** (recall at task start, remember durable facts like the vault
location), **Skills & tracking** (read SKILL.md, track via pearls),
**Environment** (workspace/vault/always-on), and **Judgment** (sensible
defaults vs ask, confirm destructive/outward-facing actions, secrets,
honest reporting). The two hard-won fixes — personal-assistant-not-support
and no reasoning narration — stay first and firm for the fast local model.
