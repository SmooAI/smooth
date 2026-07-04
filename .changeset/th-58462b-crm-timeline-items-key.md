---
"@smooai/smooth": patch
---

`th api crm timeline` (and the timeline appended under `th api crm deals show`) always rendered "no activity yet". The renderer read the event array from a top-level array or an `events` key, but the API returns `{ dealId, items: [...] }` — so `items` was never read and every timeline looked empty, including deal lifecycle events (`deal_won`, `deal_lost`, `deal_stage_changed`). Now reads `items` (bare-array + legacy `events` fallbacks kept).
