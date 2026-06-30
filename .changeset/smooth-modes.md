---
'smooai-smooth-daemon': minor
'smooai-smooth-web': minor
'smooai-smooth-code': minor
---

Smooth Modes: in-chat model switching with always-on cost (th-f512b1, th-2a6330).
`/smooth-mode <preset>` switches Big Smooth's model per-conversation across a
budget tier (flash/code/ui/plan/fast — all <$0.60/1M) and an opt-in premium tier
(flash+/code+/ui+/plan+/max). The active mode, model, cost badge, and live session
spend are shown at all times — identically in smooth-web and the `th code` TUI —
with a persistent ⚠ warning + one-time confirm on premium tiers. Per-turn model
override rides `send_message.model`; live cost rides `eventual_response.data.data.usage`;
badges come from `GET /admin/model-costs` (real gateway pricing).
