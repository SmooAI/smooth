---
'@smooai/smooth': minor
---

SMOODEV-4208: `smoo api keys` follows the ADR-162 key lifecycle. `create` takes `--name` and `--expires-in-days`, `list` shows name, status, last used and expiry (`--all` adds revoked keys), and the new `rename`, `disable`, `enable`, `disable-all` and `enable-all` commands pause keys one at a time or org-wide. `rotate` now swaps the secret in place with an overlap window (`--overlap-hours`) instead of minting a new client.
