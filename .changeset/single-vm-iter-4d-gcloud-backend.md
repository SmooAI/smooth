---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 2 iter-4d. `GcloudBackend` wraps
`gcloud auth print-access-token` to mint OAuth access tokens
for in-sandbox GCP calls.

Default globs:

* `gcr.io`, `*.gcr.io` — Container Registry
* `*.pkg.dev` — Artifact Registry (regional hosts like
  `us-central1-docker.pkg.dev`)
* `*.googleapis.com` — every Google Cloud API

`ScopeHint` is ignored — the token's IAM permissions decide
read vs write. Output is the raw token; we use the literal
`oauth2accesstoken` as the username (matches Google's
container-registry credential helper convention).

Error mapping:

* stderr containing "credentials" + "not" →
  `NotReady` ("gcloud CLI not logged in: …").
* empty stdout → `Mint`.
* other CLI failures → `Mint` with the trimmed stderr.

6 new tests: default globs, override, happy-path token,
logged-out → NotReady, empty token → Mint, generic failure
→ Mint.
