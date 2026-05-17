---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 4 iter-6a. First cleanup slice — drops
the "boardroom" framing from the user-facing surfaces while
keeping the legacy names alive for back-compat during the
transition.

Changes:

* New `smooth_bigsmooth::Narc` re-export — alias for
  `BoardroomNarc`. New code in single-VM-mode paths should
  reference `Narc`; the struct itself stays at its current
  module path so existing imports keep working.
* New env var `SMOOTH_VM_MODE` — preferred over
  `SMOOTH_BOARDROOM_MODE`. `server::start` honors either
  during the transition (new wins when both set).
* `boardroom` binary now sets both `SMOOTH_VM_MODE=1` and
  `SMOOTH_BOARDROOM_MODE=1` on startup so the binary works
  with both old and new flag readers.
* `Dockerfile.smooth-vm` exports both env vars so a container
  built before Phase 4 lands fully still satisfies any
  legacy check.
* Log message in `server::start` rephrased: "Big Smooth
  running with in-process cast" — drops the
  "Boardroom mode" framing.

No type renames yet — `BoardroomNarc`, `BoardroomHandles`,
`crate::boardroom::*` all stay where they are. Renaming the
types is iter-6b once we're confident the aliases haven't
broken anything.

271 bigsmooth tests still pass.
