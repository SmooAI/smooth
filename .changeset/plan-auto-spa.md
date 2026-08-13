---
'@smooai/smooth': patch
---

Big Smooth SPA: Plan⇄Auto mode support in the web control surface (Part 2 of the coding-harness glow-up; the daemon side is #430). A Plan/Auto toggle chip sits above the composer (amber in Plan, neutral in Auto) and POSTs `/api/session/mode` for the active conversation; Shift+Alt (⇧⌥) toggles it, and the mode re-syncs on conversation switch/reconnect via `GET /api/session/mode`. The turn's terminal `eventual_response.directive` now also carries `present_plan` (rendered as an accept/revise card — Accept flips to Auto and sends "Proceed with the plan.", Revise keeps Plan and refocuses the composer) and `todos` (rendered as a live ✔/▶/○ checklist panel above the transcript, replaced each turn), parsed the same way the existing `send_file` directive is. Todo normalization is a pure, unit-tested helper.
