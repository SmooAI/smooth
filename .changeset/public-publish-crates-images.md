---
"@smooai/smooth": minor
---

Go public: auto-publish Rust crates to crates.io and OCI images to
GHCR on every release.

- **Crates.io**: 11 library crates (`smooai-smooth-policy`, `-operator`,
  `-bootstrap-bill`, `-pearls`, `-narc`, `-scribe`, `-plugin`, `-goalie`,
  `-diver`, `-archivist`, `-wonk`) now publish in dependency-topological
  order via `pnpm ci:publish` on version-PR merge. Idempotent — re-runs
  skip crates whose target version already exists on the sparse index.
  `smooth-web` / `smooth-bigsmooth` / `smooth-code` / `smooth-cli` /
  `smooth-operator-runner` are marked `publish = false` for now; the
  first three need a web/dist include fix, the binaries ship as tarballs.

- **GHCR**: `smooai/smooth-operator` and `smooai/boardroom` images are
  built on `ubuntu-24.04-arm` (native linux/arm64, avoiding qemu
  emulation) and pushed to `ghcr.io/smooai/*` with both the release
  version tag and `:latest`. Uses the Actions-default `GITHUB_TOKEN`
  (has `write:packages` scope automatically).

- **sync-versions.mjs fixes**: the old script regex matched `smooth-*`
  when everything was renamed to `smooai-smooth-*` in commit `933b927`,
  so Cargo.lock was silently never updated. Workspace.dependencies
  smooth-X entries had hand-maintained version fields (some pinned to
  `0.2.0`, most missing entirely). Now every entry gets a synced
  `version = "x.y.z"` automatically.

- **ci:version vs ci:publish**: `changesets/action` was running the
  default `changeset version` directly, so Cargo.toml + Cargo.lock
  bumps happened only in the downstream `publish` step — too late for
  the version PR. Split into `pnpm ci:version` (changesets/action's
  new `version` input) and `pnpm ci:publish` (still the post-merge
  `publish` input, now actually publishes crates).

New secret required: `SMOOAI_CARGO_REGISTRY_TOKEN` in the repo's
Actions secrets (scope: read + publish for the `smooai-smooth-*`
prefix). GITHUB_TOKEN covers GHCR pushes automatically.
