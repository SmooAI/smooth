---
'@smooai/smooth': patch
---

wonk/narc: ground the Claude-Code-style auto-mode permission model.
`smooth_narc::judge::Decision` gains a fourth `Ask` variant with a new
`scope_options: Vec<Scope>` field on `JudgeDecision` carrying the
ladder (`Once` / `Session` / `PearlProject` / `User`) that the UI may
offer the human. Legacy `EscalateToHuman` remains as the no-hint
fail-closed form. New `smooth_bigsmooth::access::AccessStore` holds
pending requests, broadcasts `AccessEvent`s for SSE consumers, and
hands the caller a future that fires when a human resolves the
request. Pearl th-49b4aa (Phase A) — TUI wiring + HTTP routes land in
the dependent pearls.
