---
'@smooai/smooth': patch
---

`th org switch <name>` now resolves a child org you manage as a parent-org admin, as a **lazy fallback** — tried only when the name doesn't match one of your member orgs (a UUID still switches directly). This covers a parent-org admin who isn't a direct member of the child; the common path (member match or UUID) pays nothing, so there's no per-org relationships scan on every switch. (`th org list` is intentionally left as-is: surfacing children there meant an N-per-org relationships call, and in practice the managed children are usually already member orgs and thus already listed.)
