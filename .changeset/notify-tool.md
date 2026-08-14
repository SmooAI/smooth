---
'@smooai/smooth': patch
---

Add a `notify` tool so Big Smooth can proactively push to the user's devices. The agent calls `notify({title, body, deepLink?, audience?})` and the daemon fans it out over the existing web-push + platform self-notify infra (`TurnNotifier`, the same path scheduled turns use). It's injected per-turn in `tools_for` and gated by the deny-by-default family filter (ADR-008): a scoped/child principal only gets the tool if its role allowlists `notify`, and even then may only notify itself — the audience clearance is bound from the authenticated role, never tool args. A persona line steers the model to notify only for reminders, long-job completion, and genuinely useful heads-ups, never chatter. (Mobile APNs/FCM receive side is out of scope — smooai repo.)
