# @smooai/smooth

## 0.5.2

### Patch Changes

- fac7b49: Add grandiose READMEs to the 8 crates that didn't have one: `smooth-narc`,
  `smooth-scribe`, `smooth-plugin`, `smooth-goalie`, `smooth-diver`,
  `smooth-archivist`, `smooth-wonk`, `smooth-bootstrap-bill`.

  Each README follows the cast-lore voice of the main repo — centered
  banner, tagline that names the character's role, badges, one-paragraph
  "why this exists", key types, and a minimal usage example. All eight
  now render proper marketing-quality pages on crates.io rather than the
  blank no-README placeholder.

  `readme = "README.md"` added to each Cargo.toml so the file lands in
  the published crate tarball.

## 0.5.1

### Patch Changes

- ba89132: GHCR images job: log in with `GH_PAT` instead of the default
  `GITHUB_TOKEN`. The initial image pushes were done from a local
  docker login, so the packages are tied to the user account rather
  than the workflow — GITHUB_TOKEN hits `denied: permission_denied:
write_package` on subsequent CI pushes. `GH_PAT` has write:packages
  on the SmooAI org so the workflow can keep updating the existing
  packages.
- f2bb6ad: Release workflow fixes after the first `pnpm ci:publish` run:

  - **Crates.io new-crate rate limit.** Publishing 8 never-before-seen
    crates in a row tripped crates.io's new-crate rate limit on
    `smooai-smooth-diver`. `ci-publish.mjs` now sleeps 15s between
    publishes when the previous one was a first-ever upload. Version
    bumps of already-published crates publish back-to-back as before
    (that limit is far more generous).

  - **GHCR image job zigbuild deps.** The OCI-image job called
    `scripts/build-operator-runner.sh` which requires `cargo-zigbuild`
    - `ziglang`. Now installed explicitly in the job. Also added
      `libicu-dev` + `setup-go` for `smooth-dolt`, which the boardroom
      image bundles.

## 0.5.0

### Minor Changes

- 1debf51: Go public: auto-publish Rust crates to crates.io and OCI images to
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

## 0.4.4

### Patch Changes

- 1c7597a: Release workflow: build `aarch64-unknown-linux-gnu` on a native ARM
  runner instead of cross-compiling from x86_64.

  Cross-compilation was failing at `pkg_config failed: pkg-config has
not been configured to support cross-compilation` — libdbus-sys's
  build script needs per-architecture pkg-config sysroot + prefix vars,
  which are annoying to set correctly and fragile across dep updates.

  `ubuntu-24.04-arm` is now a free GitHub-hosted runner, so we switch
  the aarch64 Linux matrix entry to it. That makes the build a plain
  native build: same `libdbus-1-dev` + `libcap-ng-dev` apt deps, no
  multi-arch, no cross linker env, no sysroot juggling.

  Also removed the now-unused `gcc-aarch64-linux-gnu` install step and
  the `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` env override.

## 0.4.3

### Patch Changes

- 94ca7d2: Release workflow: add `libcap-ng-dev` to the Linux runner deps.

  After `libdbus-1-dev` unblocked compilation, the link step failed with
  `rust-lld: error: unable to find library -lcap-ng` on both Linux
  targets. `microsandbox`'s Linux-only `msb_krun_devices` uses libcap-ng
  for CAP\_\* capability management in the VM host shim, so the headers
  need to be present at link time.

## 0.4.2

### Patch Changes

- 272e90f: Release workflow: install `libdbus-1-dev` on Linux runners.

  `libdbus-sys` (pulled in transitively via the keyring / zbus chain
  that microsandbox depends on) runs `pkg-config` at build time and
  fails with "pkg_config failed" if the dev headers are missing. Both
  `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` jobs were
  failing there.

  Also separated the cross-compile toolchain install (aarch64 only)
  from the common Linux build deps step.

## 0.4.1

### Patch Changes

- 38bfd54: Release workflow: drop `x86_64-apple-darwin` from the build matrix
  and set `fail-fast: false`.

  Intel macOS has been blocking every release since microsandbox was
  wired into smooth-bigsmooth: the upstream `msb_krun_utils` v0.1.9
  crate references `kvm_bindings::kvm_irq_routing_entry` without a
  `cfg(target_os = "linux")` guard, so it fails to compile on any
  non-Linux target. On Apple Silicon the build never gets that far
  because different HVF code paths are used, but Intel macOS hits the
  wall every time.

  Until upstream gates that type properly, we ship:

  - `aarch64-apple-darwin`
  - `x86_64-unknown-linux-gnu`
  - `aarch64-unknown-linux-gnu`

  `fail-fast: false` also means a future single-platform regression
  won't silently cancel sibling builds, so we can ship the remaining
  targets while we fix the broken one.

## 0.4.0

### Minor Changes

- e7e533e: MCP servers, CLI-wrapper plugins, and project-scoped config.

  - `th mcp add/list/remove/test/path` for stdio MCP servers (Playwright,
    GitHub, filesystem, etc.). Servers register as `<server>.<tool>`.
  - `th plugin init/list/path/remove` for file-based CLI-wrapper tools at
    `.smooth/plugins/<name>/plugin.toml`. Plugins register as
    `plugin.<name>` and run the configured shell command with
    `{{placeholder}}` substitution.
  - Both MCP and plugins resolve from `~/.smooth/` (global) and
    `<repo>/.smooth/` (project); project entries shadow global on
    name collision. `--project` flags on `add` / `init` / `remove` /
    `path` scope to the repo.
  - No trust gate on loading configs — Narc screens every tool call
    (CliGuard, injection, secrets, LLM judge) and the microVM contains
    the blast radius. See `SECURITY.md` and `docs/extending.md`.

- 3657db9: Chat gets sessions. The smooth-web chat page now lists prior
  sessions in a sidebar, each persisting its own message history in
  the Dolt `session_messages` table. New `/api/chat/sessions`
  endpoints (create/list/get/delete + messages). The LLM receives the
  last 50 messages on every turn so multi-turn context is preserved.
  Session titles auto-rename to the first 60 chars of the opening
  prompt. Chat layout fixed so the input row sits flush to the
  viewport bottom (no stray scroll).
- 3657db9: Retire SQLite. Pearls, sessions, memories, config, and worker metadata
  all live in the Dolt store at `~/.smooth/dolt/` (home) or
  `<repo>/.smooth/dolt/` (per-project). `smooth.db` is gone; the
  dashboard reads "Dolt store (pearls + config)" instead of
  "Database (SQLite)". `th pearls migrate-from-sqlite` removed —
  transitional tool, no longer needed.
- 3657db9: Run in sandbox — the agent does its work in a microVM, you review it live.

  - `smooai/smooth-operator` image (unified — agent installs toolchains at
    runtime via `mise`; covers node/python/rust/go/bun/deno/~140 more).
  - `th run [pearl-id] [--keep-alive] [--image ...] [--memory-mb N]` —
    dispatches via `/api/tasks`, streams SSE events, optionally keeps the
    VM alive for dev-server review.
  - `th operators list / kill <id>` — see and tear down running VMs.
  - `th cache list / prune / path / clear` — project-scoped sandbox
    cache at `~/.smooth/project-cache/<name>-<hash>/`, bind-mounted
    at `/opt/smooth/cache` inside the VM. LRU prune by mtime.
  - Auto-forward common dev-server ports (3000, 3001, 4000, 4200, 5000,
    5173, 8000, 8080, 8888) on keep-alive runs; print reachable
    `http://localhost:<host>` URLs after the agent completes.
  - Per-run memory override threaded through
    `TaskRequest → SandboxConfig`.

- 4f91014: `th service` — background service wrapper for `th up`.

  User-level install by default on all three platforms:

  - **macOS**: writes `~/Library/LaunchAgents/com.smooai.smooth.plist`,
    drives `launchctl bootstrap gui/$UID`.
  - **Linux**: writes `~/.config/systemd/user/smooth.service`, drives
    `systemctl --user enable --now`. Prints a hint to run
    `loginctl enable-linger` so the service survives logout.
  - **Windows**: creates a logon-triggered Scheduled Task via `schtasks`.

  Commands: `install [--system]`, `uninstall`, `start`, `stop`, `restart`,
  `status`, `logs [-f]`. `--system` prints the system-level artifact +
  install instructions to stdout instead of running under sudo.

  Logs stream to `~/.smooth/service.log` and `~/.smooth/service.err`.

### Patch Changes

- 11f0c00: Fix 5 cast_integration tests that had been failing in CI since the
  Wonk bearer-token auth was added in `f7676d8`. The release workflow
  has been red for ~8 days, stranding 12 changesets and blocking every
  version bump.

  Root cause: `ALLOW_EXAMPLE_POLICY` has `[auth] token = "test-token"`,
  so Wonk's `require_operator_token` middleware rejects any request
  without `Authorization: Bearer test-token` with a 401 (empty body).
  The tests built `reqwest::Client::new()` directly and called
  `.post(...).json(...).send().await.unwrap().json().await.unwrap()`,
  which panicked at the final `.json()` with
  `reqwest::Error { kind: Decode, source: Error("EOF while parsing a value") }`.

  Fix: introduce `TEST_AUTH_TOKEN = "test-token"` next to the policy
  fixture, attach `.bearer_auth(TEST_AUTH_TOKEN)` to every direct Wonk
  request, and switch `spawn_goalie` to `WonkClient::with_auth` so its
  `/check/*` calls carry the header too. The `goalie_forwards_..._for_allowed_request`
  test had surfaced as a `403 != 200` assertion for the same reason —
  Goalie was failing its auth to Wonk and correctly denying the request.

  Narc / Scribe / Archivist tests were never affected (those services
  do not require auth).

- 3b1a88a: Diagnostic logging on the sandboxed dispatch path so we can tell
  _why_ `th run` / `th code --headless` fail when they do:

  - `bill::exec_sandbox` logs exec start + non-zero-exit with
    captured stdout/stderr tails (was silent before, making code=-1
    failures opaque).
  - Dispatch handler now runs a preflight `/bin/sh` probe against
    the sandbox before exec-ing the runner — surfaces whether
    bind-mounts landed, whether the runner binary is visible + executable,
    and what the guest's `/opt` actually contains.

  Pearl `th-1ec3ce` (P0) tracks the underlying issue: on plain alpine,
  microsandbox's bind-mounts aren't reaching the guest at all, so
  every sandboxed dispatch fails with `exit=-1 stderr=""`. Fix requires
  digging into microsandbox's mount-arg plumbing; these changes just
  give us the visibility to do it.

- 72f7eef: Fix P0 (th-1ec3ce): sandbox bind-mounts not landing in the guest VM.

  The microsandbox guest agent does not `mkdir -p` mount targets before
  calling `mount -t virtiofs` — mounts to paths that don't pre-exist in
  the rootfs (`/opt/smooth/bin`, `/opt/smooth/policy`, `/workspace`)
  silently fail. We were falling back to plain `alpine` because our
  custom `smooth-operator` image was only in Docker Desktop's local
  store and microsandbox couldn't pull it; alpine has an empty `/opt`,
  so every mount missed.

  Fix: publish `smooai/smooth-operator` and `smooai/boardroom` images
  to GitHub Container Registry (public), and default to pulling from
  there. The `Dockerfile.smooth-operator` pre-creates `/workspace`,
  `/opt/smooth/bin`, and `/opt/smooth/cache/mise` — so every bind-mount
  target now exists before the guest agent tries to mount on top of it.

  - `SandboxConfig` default image: `alpine` → `ghcr.io/smooai/smooth-operator:latest`
  - `th run` default: `smooai/smooth-operator:latest` → `ghcr.io/smooai/smooth-operator:latest`
  - `scripts/build-smooth-operator-image.sh` + `build-boardroom-image.sh`:
    default `SMOOTH_IMAGE_REPO` to `ghcr.io/smooai/...`, add `--push`
    flag so one command builds + publishes.
  - Preflight probe now confirms mounts land: `/opt/smooth/bin/smooth-operator-runner`
    is executable inside the VM and the runner boots as expected.

  Users can override with `SMOOTH_WORKER_IMAGE` / `SMOOTH_OPERATOR_IMAGE`
  if they publish a fork to a different registry. Public pulls from
  `ghcr.io/smooai/*` require no auth.

- 3657db9: Buildable OCI images for the microVM cast:

  - `docker/Dockerfile.smooth-operator` + `scripts/build-smooth-operator-image.sh`
    → `smooai/smooth-operator:<version>` (alpine + mise + runner).
  - `docker/Dockerfile.boardroom` + `scripts/build-boardroom-image.sh`
    → `smooai/boardroom:<version>` (alpine + boardroom bin + smooth-dolt).
  - Both scripts delegate to the existing cross-compile flow
    (`build-operator-runner.sh` / `build-boardroom.sh`).
  - Fixed a latent package-name bug in `build-boardroom.sh`
    (`-p smooth-bigsmooth` → `smooai-smooth-bigsmooth`).

  Still pending: registry publish on release so `microsandbox` can
  pull without Docker on end-user machines.

- 3657db9: Pearl fixes:

  - `/api/pearls` + `/api/projects/pearls` default to unbounded
    (`?limit=0`). The dashboard was silently capped at 100 — a repo
    with 150+ pearls showed "100 closed, 0 open" when the open ones
    were past the limit. LLM tool callers still get a 100-row
    default via `list_pearls()`.
  - `PearlStore::close` is now invoked on every error-path exit of
    the sandboxed dispatch handler (runner not found, workspace
    create failed, LLM provider missing, runner exited non-zero).
    Previously only exit-0 closed the pearl; leaked `Task: ...`
    pearls accumulated from E2E runs.
  - `install:th` now re-adhoc-signs a neighbor `smooth-dolt` binary
    in `~/.cargo/bin/` so `scp`'d copies work under `launchd`
    without a manual `codesign`.

- 3657db9: `th doctor --init-home-repo` scaffolds `~/.smooth/` as a git repo for
  backup / cross-machine sync. Writes a `.gitignore` that excludes
  secrets (`providers.json`), service logs, audit logs, the Dolt
  store (has its own push/pull), the project cache, and ephemeral
  debug captures. Idempotent. Optional `--remote <url>` adds origin.
- 3657db9: Wordmark + cast renames in user-facing surfaces:

  - `th` CLI + web dashboard now say "Big Smooth" (not "Leader") and
    "Smooth Operators" (not "Sandbox").
  - New horizontal logo (`images/logo.png`, `crates/smooth-web/web/public/logo.svg`)
    replaces the old mark.
  - `th up` / `th status` / `th doctor` banners render "Smooth" with
    the logo's gradient colors via ANSI 24-bit truecolor escapes
    ("Smoo" orange→pink, "th" teal→blue).
  - `/health` service field renamed `smooth-leader` → `big-smooth`.
  - `SMOOTH_SANDBOX_MAX_CONCURRENCY` env + `th up --max-operators N`
    flag expose the previously hardcoded pool cap of 3.

- 3b1a88a: Wordmark gradient (`"Smoo"` orange→pink, `"th"` teal→blue) live in
  `th up` / `th status` / `th doctor` banners via a new
  `gradient` module in smooth-cli. Helper `smooth()` stitches the
  two halves into the full word; `smoo_ai()` covers the Smoo AI
  brand. Pure ANSI 24-bit truecolor, no new deps. 4 unit tests.

## 0.3.0

### Minor Changes

- 799c5ca: Major session: Changesets CI, web dashboard, provider overhaul, port forwarding, security enforcement.

  - Changesets versioning with auto Cargo.toml sync and GitHub Actions release pipeline
  - Husky git hooks with pre-commit cargo checks
  - th up daemon mode (background process with pid file), th down kills it
  - Interactive th auth login with provider/model picker and connection test
  - Kimi + Kimi Code providers (OpenAI-compat and Anthropic-compat)
  - Removed deprecated OpenCode Zen module entirely
  - Web dashboard: shadcn UI components, Tailwind v4 dark theme, responsive sidebar
  - Multi-project pearl support: /api/projects, project switcher, per-project pearl views
  - Pearl kanban with search, timeline view, stats view with bar charts
  - System topology SVG graph (radial layout, pulsing nodes, auto-refresh)
  - Clickable dashboard cards navigating to system/pearls
  - Port forwarding Phase 1: PortPolicy, forward_port tool, Wonk /check/port
  - VM path mapping: guest→host translation for filesystem deny pattern enforcement
  - Rich ANSI colors across CLI (th up/down/status/auth/doctor)
  - Doctor checks on th code startup

## 0.2.1

### Patch Changes

- a213b90: Remove deprecated OpenCode Zen module and all references. Add Kimi Code as a provider. Chat handler now uses ProviderRegistry instead of hardcoded OpenCode Zen API.

## 0.2.0

### Minor Changes

- 5b31872: Add Changesets versioning with automated version sync to Cargo workspace. Includes sync-versions script, CI publish workflow, and multi-platform binary release on version bump.
