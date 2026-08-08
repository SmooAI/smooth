---
'@smooai/smooth': patch
---

`th config set` no longer silently corrupts number-shaped values (pearl th-a5fc9e).

Values were JSON-parsed whenever they parsed, so an all-digit token became an f64 and lost everything past ~17 significant figures. Four live values are already corrupted and unrecoverable, since every stored copy is lossy: `slackClientId` (`10574252146965.107` — ten fractional digits gone), `derekDettmanSparkAgentKey`, `derekDettmanSparkOfficeKey`, and `sparkAgentKey` in Derek's org.

Secrets are now never parsed — a secret is an opaque token whose bytes must round-trip exactly. Every other tier must round-trip: the parsed value is re-serialized and compared to the input, and a mismatch is refused rather than stored. That second rule is the one that matters, because `slackClientId` is public tier and a secrets-only rule would have missed it. Pass `--string` to store a number-shaped identifier verbatim. Feature flags, limits and ordinary numbers round-trip, so they still parse as before.

