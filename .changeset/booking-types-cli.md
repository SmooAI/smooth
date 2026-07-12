---
"@smooai/smooth": patch
---

th booking: add booking-types CRUD (`th booking types list|create|update|rm`) against the `/booking/types/{orgId}` endpoints, with `--durations`, `--note`, `--conferencing`, `--window-start/--window-end`, `--one-time`, and `--org-shared`. `config set` gains `--slug` (public link handle), `--conferencing`, and `--avatar-url`; `link` now prefers the config's link handle over the member email and supports `--type <slug>` (typed link) and `--note <text>` (ad-hoc pre-fill). Plural/singular command forms are now interchangeable via clap aliases: `booking`⇄`bookings`, `types`⇄`type`, `block`⇄`blocks`, `calendars`⇄`calendar`. Parity with the shipped monorepo booking features (SMOODEV-2443/2518/2528).
