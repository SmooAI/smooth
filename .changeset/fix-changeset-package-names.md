---
"@smooai/smooth": patch
---

release: align all changeset package names to `@smooai/smooth`

`pnpm changeset version` (the Release workflow's Version Update step) was
failing repo-wide with "Found changeset … for package X which is not in the
workspace": 35 accumulated changeset files declared the package under three
wrong spellings — `smooai-smooth` (21), bare `smooth` (12), and
`@smooai/smooth-cli` (2) — none of which exist in the workspace. The only
package is the root `@smooai/smooth` (per `package.json`). Renamed every
changeset to `@smooai/smooth` so versioning can run and the backlog of
pending changesets (including the pearls auto-heal + test-hygiene fixes)
finally gets a version bump + changelog entry. Pearl th-645e54.
