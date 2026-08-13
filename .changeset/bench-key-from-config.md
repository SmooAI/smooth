---
'@smooai/smooth': patch
---

Read the bench gateway key from `@smooai/config` instead of a GitHub secret that never existed.

Three workflows — `bench-engines.yml`, `bench-models.yml`, `the-line.yml` — referenced
`secrets.SMOOAI_GATEWAY_API_KEY`, which is present in neither the repo nor the org. All three have
been scoring with an empty key. `the-line.yml` carried a second, independent bug: it spelled the
variable `SMOOAI_GATEWAY_API_KEY` while `smooth-bench` reads `SMOOAI_GATEWAY_KEY`, so it would have
run keyless even once the secret existed.

Copying a gateway key into CI was the wrong fix anyway. All three now authenticate with one M2M
bootstrap pair (`SMOOAI_CLIENT_ID` / `SMOOAI_CLIENT_SECRET`) and resolve `smooaiLlmKey` through
`th config` — the same source of truth the rest of the monorepo uses. Rotating the gateway key no
longer touches CI, and the key is masked before it can reach a log.

Each workflow now fails in seconds with the real reason when the credential is missing, rather than
producing a full run of zeros that reads as five broken engines or a model regression.
