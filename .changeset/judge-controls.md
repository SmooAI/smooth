---
'@smooai/smooth': patch
---

Give the user control over Big Smooth's safety judge (the Narc hook).

Narc's LLM judge — the fail-closed second opinion that adjudicates ambiguous tool calls (prompt
injection, data-destroying writes, secret exfiltration) — used to be always-on with a fixed fast
model and no way to tune it. It now has three runtime knobs, shared between the `NarcHook` and a new
`GET`/`POST /api/judge` route and surfaced in a "Safety judge" section on the Settings page:

- **enable/disable** the LLM judge. Off removes only the LLM-escalation tier — the permission gate's
  DenyPolicy circuit-breakers, Narc's hard-signal detectors (dangerous-CLI hard block, unambiguous
  `Block`-severity destruction/exfiltration), the effect-based shell restore, and secret redaction
  all keep running. It is degraded, never open.
- **strictness** (lenient / normal / strict) — which detector severities escalate to the judge vs.
  alert-only, and how hard the daemon fails closed when no judge is reachable. Lenient judges only
  hard signals; strict blocks even ambiguous hits when the judge is unavailable.
- **judge model** — the model the judge runs as, selectable independently of the chat model (the
  first "role slot"; defaults to the daemon's fast model).

Every detector routes through one `decide(strictness, severity, judge_available)` gate, so the
posture is defined in one exhaustively-tested place. Security-critical, so the tests cover the full
level × severity × judge-availability matrix and assert that a disabled judge still hard-blocks
`rm -rf /` and unambiguous data destruction.
