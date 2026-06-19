---
"@smooai/smooth": patch
---

release: stop publishing internal crates to crates.io; ship `th` as a binary only

The release workflow was wired to `cargo publish` the entire workspace to
crates.io, but those crates are internal pieces of the `th` binary — every
cross-crate dependency is a workspace `path` dep, so nothing consumes them
from the registry. The product is the `th` binary (GitHub release assets), and
the only genuinely-public crate, `smooai-smooth-operator-core`, is published
from its own repo. The first real publish run had already pushed one internal
crate (`smooai-smooth-policy@0.14.0`) before aborting on a stale publish list.

Changes: mark all 13 publishable workspace crates `publish = false`; drop the
`publish:` / `createGithubReleases:` wiring from the Release workflow (the
version PR + binary build matrix are gated on the version-bump merge commit, so
the binary release is unaffected); and empty `ci-publish.mjs`'s crate list to a
no-op. `smooth-policy@0.14.0` is yanked from crates.io out-of-band. Pearl
th-607f69.
