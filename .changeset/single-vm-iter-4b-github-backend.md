---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 2 iter-4b. First concrete host-stub
backend: `GitHubBackend` wraps `gh auth token`.

Default globs cover `github.com`, `*.github.com`, `ghcr.io`,
and `npm.pkg.github.com`. `with_globs(...)` lets users
override for GitHub Enterprise installs.

Per-issue flow:

1. Run `gh auth status` first to surface a clean
   `NotReady` (with the user-facing "not logged in" message)
   instead of an opaque mint failure when the user has
   logged out.
2. Run `gh auth token` and trim the stdout. Empty output
   maps to `Mint` so the sandbox sees a concrete error
   rather than an empty secret.

`info().ready` stays `true` — we don't want `info()` to
shell out on every list call. The TUI's readiness pane gets
a dedicated probe in a follow-up iter.

`CommandRunner` trait abstracts the shellout so tests
inject a `StubRunner` with canned `gh` outputs (no need for
a real `gh` binary on the test host).

6 new tests: default-globs check, custom-globs override,
happy-path issue, logged-out → NotReady, empty token →
Mint, `gh auth token` failure → Mint.
