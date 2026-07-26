---
'@smooai/smooth': minor
---

`th api dashboard layout get|add|remove` — manage your Smoo AI main-dashboard
widget layout from the CLI (SMOODEV-2753).

Layouts are per (user, org, dashboard type) rows behind
`GET/PUT /organizations/{org}/dashboard/layout`, previously reachable only by
clicking around the web dashboard or hand-rolling curl against a user JWT.
`layout add <widget_id>` does a read-modify-write: fetches the saved layout,
appends the widget below the current grid with the size→span mapping the web
uses (small 3 / medium 6 / large 9 / full 12 on the 12-column grid), and PUTs
it back — the server validates the widget id against its registry, so a typo
fails loudly instead of saving a dead tile. `layout remove` is the inverse;
`layout get` prints the saved (or default) layout as JSON. Rides the user JWT
(`th auth login`) because layout rows are keyed by user id — M2M tokens can't
own a layout. Dogfood: added the new `aws_cost_forecast` widget to the Smoo AI
org dashboard with `th api dashboard layout add aws_cost_forecast`.
