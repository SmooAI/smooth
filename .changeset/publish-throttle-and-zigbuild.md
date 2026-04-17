---
"@smooai/smooth": patch
---

Release workflow fixes after the first `pnpm ci:publish` run:

- **Crates.io new-crate rate limit.** Publishing 8 never-before-seen
  crates in a row tripped crates.io's new-crate rate limit on
  `smooai-smooth-diver`. `ci-publish.mjs` now sleeps 15s between
  publishes when the previous one was a first-ever upload. Version
  bumps of already-published crates publish back-to-back as before
  (that limit is far more generous).

- **GHCR image job zigbuild deps.** The OCI-image job called
  `scripts/build-operator-runner.sh` which requires `cargo-zigbuild`
  + `ziglang`. Now installed explicitly in the job. Also added
  `libicu-dev` + `setup-go` for `smooth-dolt`, which the boardroom
  image bundles.
