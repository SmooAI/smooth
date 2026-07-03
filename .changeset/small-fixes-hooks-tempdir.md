---
'@smooai/smooth': patch
'@smooai/smooth': patch
---

Two small bug fixes.

**th-9550e6 — `th pearls init` no longer clobbers husky's git hooks.** Hook
install now refuses to overwrite a foreign `core.hooksPath` (e.g. husky's
`.husky/_`): if one is already set to anything other than `.githooks`, it
leaves it untouched and prints a note instead of writing smooth's Rust hooks
and disabling the repo's own lint-staged/prettier. Installing still proceeds
when `core.hooksPath` is unset or already points at `.githooks`. Applies to
`th pearls init`, `th hooks install`, and `th doctor`'s auto-fix.

**th-8bfbf4 — bigsmooth `is_invalid_project` filters temp dirs cross-platform.**
The registry filter matched only the macOS `/var/folders` tempdir prefix, so
Linux CI tempdirs (`/tmp/...`, `/run/user/.../...`) accumulated in
`~/.smooth/registry.json` and each spawned a phantom `smooth-dolt serve` at
startup. It now checks `std::env::temp_dir()` (canonicalized) plus explicit
`/tmp`, `/private/tmp`, `/var/folders`, `/private/var/folders`, and `/run/user`
fallbacks, using component-wise prefix matching.
