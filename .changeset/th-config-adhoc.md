---
'@smooai/smooth': patch
---

`th config set --adhoc` — ad-hoc writes for keys not declared in any org config schema.

The server has always supported `"adhoc": true` for undeclared keys (its 400 error even prescribes it), but the CLI never sent it — so release-tooling secrets (match passphrase, ASC API key material) couldn't be stored in `@smooai/config` without first inventing a consumer schema for them. `--adhoc` requires an explicit `--tier` (clap-enforced), so an ad-hoc write states its sensitivity out loud instead of inheriting a default. First use: storing the Big Smooth TestFlight signing secrets in the Smoo AI prod org.
