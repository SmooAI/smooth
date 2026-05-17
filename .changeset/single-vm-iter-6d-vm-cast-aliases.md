---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 4 iter-6d. Naming aliases — extends
iter-6a's `Narc` alias to the rest of the boardroom surface.
Existing call sites keep working unchanged; new code prefers
the cleaner names.

New aliases in `smooth_bigsmooth`:

* `vm_cast` — module alias for `boardroom`.
  `crate::vm_cast::*` and `crate::boardroom::*` resolve to
  the same items.
* `VmCastHandles` — type alias for `boardroom::BoardroomHandles`.
* `spawn_vm_cast` — fn alias for `boardroom::spawn_boardroom_cast`.

No `#[deprecated]` attrs yet — those would emit warnings on
the 91 existing call sites and trip the workspace
`-D warnings` gate. Removal of the legacy names happens in a
dedicated rename PR once new code consistently uses the new
ones.

2 new smoke tests confirm both name paths resolve to the
same items. Existing 269 bigsmooth tests still pass.

This effectively closes the "drop boardroom term" item from
Phase 4's checklist — the term is now optional everywhere
user-facing, kept as legacy compatibility under the hood.
