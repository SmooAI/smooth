---
'@smooai/smooth': patch
---

`th code` TUI: Plan⇄Auto mode support. shift+tab now toggles the bound
conversation between Plan (agent held read-only, proposes a plan) and Auto
(agent executes), POSTing the change to the daemon's `/api/session/mode` for the
same conversation the TUI sends turns with. The status line shows a PLAN (amber)
/ AUTO badge with a shift+tab hint. The `present_plan` directive renders as an
accept/revise card — an empty-draft Enter accepts (flips to Auto and sends
"Proceed with the plan."), typed feedback revises while staying in Plan. The
`todos` directive renders as a live boxed checklist (✔/▶/○) that replaces the
previous one. Both directives ride the existing `eventual_response.directive`
field.
