---
'@smooai/smooth': patch
---

`th attest` asks the repo's `scripts/ci/_<check>-untouched.mjs` path filter (exit 0 = nothing to check) before queueing, and credits the row as passed without taking a heavy slot or the cargo lock. A web-only branch no longer waits tens of minutes in the queue for `attest rust` to skip itself.
