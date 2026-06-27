---
'@smooai/smooth': patch
---

Feature-gate the `th admin` superadmin/cross-org command tree behind a non-default `admin` Cargo feature on `smooai-smooth-cli`. The public release/brew binary (built without the feature in `release.yml`) no longer ships `th admin`, since it targets `/admin/*` endpoints that require the `requireSuperAdmin` role and is not a publicly-advertised surface. Local and internal builds keep it: the root `install:th` and `install:th:full` scripts now pass `cargo install … --features admin`. The `admin` module (and its tests) only compiles with the feature; the shared `api_url()` helper was inlined into the user-JWT client so non-admin `th api` surfaces compile cleanly without the admin module.
