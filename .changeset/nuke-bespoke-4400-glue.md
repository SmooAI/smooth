---
'smooai-smooth-cli': patch
'smooai-smooth-code': patch
'smooai-smooth-policy': patch
---

EPIC th-c89c2a: scrub the last bespoke `:4400` glue now that `th daemon` is the
operator. Removed `BigSmoothClient` + the dead headless-capture/SSE paths from
smooth-code (kept the `ServerEvent`/`PriorMessage` wire types the operator client
maps onto); dropped `th access` and pointed `th model`/`th doctor` health checks at
the operator (`:8787`); deleted the orphaned `smooth-credential-helper` crate and
`smooth-policy`'s vestigial `AuthConfig.leader_url`. The tree is `:4400`-free.
