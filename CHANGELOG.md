# @smooai/smooth

## 0.15.8

### Patch Changes

- c7f484d: Bring the `/th-mail` Claude skill into the repo (git-tracked) + a symlink installer.

  - Adds `.claude/skills/th-mail/` (SKILL.md + watch-once.sh) — the harness-agnostic agent-mail watcher (`watch-once.sh` blocks until unread `th msg` mail arrives, prints it, and exits so a background task re-invokes the agent; no busy-poll; does NOT `--pull` by default to avoid the Dolt write-lock contention that caused store-wide `Error 1105: database is read only`).
  - Adds `scripts/install-skills.sh` + `pnpm install:skills`, which symlinks the repo's skills into `~/.claude/skills` (backing up any existing copy). The skill now lives in ONE git-tracked place, so it can't be silently changed by an untracked local edit. Output follows the Smooth Flow glyph vocabulary.

## 0.15.7

### Patch Changes

- c8e285e: Feature-gate the `th admin` superadmin/cross-org command tree behind a non-default `admin` Cargo feature on `smooai-smooth-cli`. The public release/brew binary (built without the feature in `release.yml`) no longer ships `th admin`, since it targets `/admin/*` endpoints that require the `requireSuperAdmin` role and is not a publicly-advertised surface. Local and internal builds keep it: the root `install:th` and `install:th:full` scripts now pass `cargo install … --features admin`. The `admin` module (and its tests) only compiles with the feature; the shared `api_url()` helper was inlined into the user-JWT client so non-admin `th api` surfaces compile cleanly without the admin module.

## 0.15.6

### Patch Changes

- 2dbd1d6: Make `th api keys` first-class for both auth-client types. `create` now takes structured `--type m2m|b2m` and repeatable `--allowed-origin` flags (B2M requires ≥1 origin, validated client-side) instead of a hand-written raw JSON body; `update <id> --allowed-origin …` replaces a B2M client's origin allowlist (PATCH, B2M-only); a new `rotate <id>` mints a replacement of the same type/origins then revokes the old one (the API has no in-place rotation, so the replacement is created first and the new client id + key are shown once). Adds accurate help (M2M secret vs B2M publishable, both shown once), `--json` on reads, and `--org-id [aliases: --org]`. The raw `--body` escape hatch stays. Fixes a latent bug: these routes require a dashboard user session (`auth.provider === 'supabase'`, 403 under M2M), so the surface now uses the user-JWT `UserClient` rather than the M2M-capable client. (pearl th-8d2a41)
- a1326b1: `th` CLI audit quick-wins (non-breaking). Standardized the org-override flag to `--org-id` with `--org` as a visible alias across every `th api *` leaf (agents, members, knowledge, jobs, products, observability, crm, testing — ~41 args), retiring the `--org` and `--organization-id` spellings. Filled ~100 previously-blank `--help` doc strings on subcommand variants and args (the whole `th api *` and `th testing *` CRUD surface, including `th testing`'s runs/cases/environments/deployments groups). Gave the operative-control commands (`th pause/resume/steer/cancel/approve`) a proper `<OPERATIVE_ID>` metavar + arg help (was the stale `<BEAD_ID>`), clarified `th inbox` vs `th msg inbox`, fixed the lowercase `th api jobs list --type` metavar, wrote accurate descriptions for `th db`/`th project`/`th jira`/`th web`/`th tailscale`, and scrubbed stale `th api orgs switch`/`--org` references from `th org switch` help. No behavior or signature changes. (pearls th-c153ec follow-up; from the th-CLI audit)

## 0.15.5

### Patch Changes

- 919e780: Add `th llm` — a top-level surface for an org's `llm.smoo.ai` gateway keys, wrapping the shipped `api.smoo.ai/organizations/{org_id}/llm-gateway/*` API: `overview`, `usage`, `create-key`, `rotate-key`, and `keys` (list/create/rotate/delete). Mints the org's persistent LiteLLM virtual key (scoped to the org's team/budget) and prints the value once. Authenticates as the user (Supabase JWT) and is org-admin-gated, so a master admin can mint for a child org with `--org-id <child>`. Adds a `delete` method to the user-JWT `UserClient`. This is the static-key model the backend actually ships — it re-scopes pearl th-f7b20f (whose ephemeral JWT→session design has no backend endpoint). (pearl th-f5781f)

## 0.15.4

### Patch Changes

- 073d279: Org DX bundle: add a top-level `th org` alias for `th api orgs` (list/switch/show) for discoverability; `th auth whoami` now prints a switch hint; `--org` and `--org-id` are interchangeable on both `th config` and `th admin config` (each accepts the other as an alias); and `docs/Engineering/Using-th-CLI.md` documents the key gotcha — the **user JWT** acts cross-org via `--org-id` (master admin over child orgs) while **M2M** tokens are org-locked server-side, so `th org switch` is cosmetic for the `--m2m`/`th admin config` surface and child-org config-env bootstrap must use the deploy path (`prepareSmooConfig`), not an admin env-create. (pearl th-c153ec; closes the active-org switch-contract work tracked in th-3217db, which round-trips cleanly across all 3 credential stores.)

## 0.15.3

### Patch Changes

- 45efc59: Add `pnpm install:th:brew` — installs the latest released `th` via Homebrew (`brew upgrade SmooAI/tools/th || brew install SmooAI/tools/th`) for anyone who just wants the published binary without a full source build. `install:th` and `install:th:full` are unchanged and still build from local source (the dev test loop). (pearl th-2bd1c8)

## 0.15.2

### Patch Changes

- 118089f: Fix the release pipeline so the Homebrew tap update (and build/release) only run on the actual version-bump merge, not the version-PR run. `check_release` keyed `should_release` off `git log -1`, but the changesets action leaves HEAD on its own `🦋 New version release` commit on the `changeset-release/main` side-branch — so the gate matched in the version-PR run too, firing build/release/update-homebrew-tap against a half-merged tree and 404'ing the tap job on a not-yet-published asset (a spurious red failure every release). Now keyed off `github.event.head_commit.message` (the commit actually pushed to main), with an `origin/main`-tip fallback for `workflow_dispatch`. README: brew install stays the headline method, with a verify step. (pearl th-891ccb)

## 0.15.1

### Patch Changes

- 12ef522: Fix pearl_comments.seq column-level heal that silently never applied. Dolt has no `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so the original heal errored on every open and the failure was swallowed as a debug log — pre-messaging stores kept a seq-less `pearl_comments` and `th pearls show` blew up with "column seq could not be found". migrate_schema now probes `information_schema.columns` via a new `column_exists` helper and runs the bare `ALTER` only when the column is genuinely absent. (pearl th-f89a3c, surfaced restoring the smooblue store.)

## 0.15.0

### Minor Changes

- f960f40: feat: harness-agnostic agent messaging (`th agent` / `th msg`) + `th pearls prime`/memories

  Any agent — Claude Code, opencode, pi, a shell loop, in any session on any
  machine — can now register an identity and exchange messages with other agents,
  all through plain `th` calls layered on the pearl Dolt store (so it syncs via
  `refs/dolt/data` like everything else). Pearls th-70aaef + th-202885.

  **Agent messaging:**

  - New Dolt tables `agents` (persistent registry) and `messages` (mailbox;
    `read_at IS NULL` = unread, `seq` for stable insertion order, `thread_id` for
    flat threads).
  - `smooth-pearls` gains `AgentRegistry` (register/touch/set_status/list/get) and
    `Mailbox` (send/inbox/sent/get/thread/mark_read/mark_all_read/unread_count).
  - New CLI: `th agent register/list/offline` and
    `th msg send/inbox/read/reply/thread/watch`. `th msg watch` is the
    "continuously check" poll loop (`--pull` for cross-machine). Identity defaults
    to `$SMOOTH_AGENT`, else `user@host`; `$SMOOTH_HARNESS` tags the tool.
  - `th inbox` (previously a stub that always returned `[]`) now aliases
    `th msg inbox` against the real local mailbox.
  - `th pearls init` injects an idempotent **Agent Messaging** section into
    `AGENTS.md` (marker-bounded) so any harness that reads it learns the protocol.

  **Prime + memories:** `th pearls remember/memories/forget` over the existing
  `memories` table, plus `th pearls prime` which prints (or `--json`) a compact
  session-priming context: in-progress + open pearls and recent memories.

  Also fixes a smooth-dolt datetime-format quirk surfaced here: `NOW()` returns
  RFC3339 while `CURRENT_TIMESTAMP` defaults are space-separated — the shared
  `parse_dolt_datetime` now accepts both.

- e8f9693: feat: harden pearls sync — auto-push mutations + fail-safe pull (no silent data loss)

  Pearls could be lost to the `refs/dolt/data` divergence: a mutation committed
  only to the local Dolt store, then a later `th pearls pull` moved `main` to the
  remote tip and orphaned the un-pushed commits. Two guards close the gap
  (pearl th-4a4559):

  - **Auto-push on mutation.** `th pearls create/update/close/reopen/dep/comment/
label/migrate` now push to the repo's `refs/dolt/data` immediately after the
    local commit — best-effort and quiet when there's no remote/offline (drives
    only `dolt push`, which captures its own output; no stray `fatal:` on stderr).
    Pearls are durable the moment they're made, so no pull or re-clone can drop
    them. `SMOOTH_PEARLS_NO_PUSH=1` opts out (bulk/scripted creates).
  - **Fail-safe `th pearls pull`.** Refuses by default when local `main` is ahead
    of `remotes/origin/main` (commits not yet on the remote), pointing you at
    `th pearls push` first; `--force`/`-f` pulls anyway. Detection fetches the
    remote and counts `dolt_log('remotes/origin/main..main')`; if it can't be
    determined (no remote / fetch fails) the guard is skipped so remote-less
    stores still pull.

  Generalizes the messaging sync helpers (`sync_push_pearl_state` /
  `sync_pull_pearl_state`, formerly `*_messaging`) since they push/pull the whole
  pearl store. Verified live: a remote-less `create` is a clean no-op, and a
  store that was 2 commits ahead correctly refused the pull.

### Patch Changes

- 641ffbf: fix: model-picker sort comparator is now a total order (unblocks releases)

  `candidate_models_filtered` sorted models by slot benchmark with
  `y.partial_cmp(&x).unwrap_or(Ordering::Equal)`. A NaN benchmark makes
  `partial_cmp` return `None`, and collapsing that to `Equal` violates total
  order — which Rust's sort (1.96+) detects and **panics** on ("comparison
  function does not correctly implement a total order"). That panic failed the
  Release workflow's `cargo test` step on CI (whose model data hit the NaN path),
  so every Release run failed, the "New version release" changeset PR never
  auto-merged, and the version sat at 0.14.1 while changesets piled up. Switched
  to `f32::total_cmp`, a proper total order that sorts NaN deterministically.
  Pearl th-03b02e.

- 2710849: feat: `th msg`/`th agent` sync over `refs/dolt/data` (push-on-send, pull-on-watch)

  Messages live in the pearl Dolt store, which syncs over the repo's git remote
  via `refs/dolt/data` — but `th msg send` previously only committed locally, so
  agents in different clones/machines of the same repo didn't see each other
  until a manual `th pearls push`. Now the messaging commands sync automatically:

  - `th msg send` / `th msg reply` / `th agent register` / `th agent offline`
    **push** after committing (`--no-push` to skip).
  - `th msg watch` **pulls** each poll by default (`--no-pull` for a local-only,
    offline mailbox).
  - `th msg inbox --pull` fetches the remote before listing.

  Sync drives only `dolt push` / `dolt pull` (which capture their own output), so
  a repo with no remote — or the global `~/.smooth/dolt` store, or being offline
  — is a silent no-op: no error, no stray `fatal: No configured push destination`
  on stderr. Pearl th-bdaaa7.

## 0.14.1

### Patch Changes

- 0645853: release: stop publishing internal crates to crates.io; ship `th` as a binary only

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

- 106594a: build: `sync-versions.mjs` also skips the external `operator-core` in Cargo.lock

  Follow-up to the Cargo.toml skip (th-1ee32b): the Cargo.lock updater matched
  `name = "smooai-smooth-operator-core"` too and bumped its locked version to the
  workspace version (0.14.1), so even with the dependency requirement corrected
  to `^0.14.0`, cargo failed with "locked to 0.14.1 … candidate 0.14.0". The lock
  updater now skips `smooai-smooth-operator-core`, leaving it pinned to its real
  published release. Pearl th-1ee32b.

- 3ae4112: build: `sync-versions.mjs` no longer bumps the external `operator-core` dep

  `scripts/sync-versions.mjs` rewrote the `version = "…"` on every
  `smooth-X = { … }` workspace.dependencies line to the workspace version —
  including `smooth-operator = { …, package = "smooai-smooth-operator-core" }`,
  the **external** agent engine published from its own repo. When the version PR
  bumped the workspace to 0.14.1, it rewrote the operator-core requirement to
  `^0.14.1`, which doesn't exist on crates.io (latest is 0.14.0), breaking
  `cargo build --examples --workspace` and the version PR's checks. The script
  now skips any workspace-dependency line that pins `smooai-smooth-operator-core`,
  leaving its requirement at the real published version. Pearl th-1ee32b.

## 0.14.0

### Minor Changes

- 61a0b81: SMOODEV-1164: `th observability sourcemaps upload <dir>` — bulk source map upload.

  New CLI surface for the Error Tracking dashboard's symbolication path.
  Walks a build directory (`.next/`, `dist/`, `.open-next/`, etc.), finds
  every `.js{,mjs,cjs}` paired with a `.map`, registers each map against
  a (release, environment) pair via the Smoo Observability API, then
  PUTs the bytes to the presigned S3 URL the API returns.

  Companion `th observability sourcemaps list` prints currently
  registered maps for a release.

  Backend half ships as SMOODEV-1164 in the smooai monorepo.

- 06800c1: coding_workflow: cleanup-intent hint plumbing for continuation turns

  The fixer's test-fix bias + cross-fixture pattern confabulation made
  `cleanup-node-modules-orphans` chronically unreliable on v4-pro
  (1/6 perfect in pane-captured samples — agents fabricating
  `packages/db/db.test.js` on cleanup tasks; running
  `find . -type f -size +150k -delete` on a node-modules orphan
  task). The existing `is_cleanup_intent(task)` preamble in
  `build_user_prompt` suppresses both failure modes — but it only
  fires when the CURRENT user message matches cleanup verbs/nouns,
  which the bench's "yes, proceed" coach reply does not.

  This change plumbs a `cleanup_intent_hint: bool` through
  `CodingWorkflowConfig`. The runner sets it by scanning
  `agent_config.prior_messages` for cleanup intent — so when the
  prior turn was a cleanup README, the workflow re-applies the
  preamble on the confirmation turn via a new `is_confirmation_reply`
  helper.

  Net result at deepseek-v4-pro:

  - `cleanup-node-modules-orphans`: prior 1/6 perfect (3/5 + 1 no-action
    - 1 catastrophic 7.2MB protected-dir delete) → **5/5 perfect,
      zero-variance identical 3,559,394 bytes**. Matches opencode's
      3/3 identical-bytes baseline on the same fixture.
  - `cleanup-disk-bloat`: 3/3 → ~2/3 (~67% pass rate; one cross-fixture
    hallucination remained). Net regression on this fixture.
  - `cleanup-impossible-task`: 3/3 → variance not yet characterized,
    early sample 1/2.
  - `cleanup-pycache-debris`: 3/3 strong → 2/2 stable.

  Trade-off worth shipping: eliminating the chronic
  catastrophic-delete failure mode on node-modules (a fixture where
  v4-pro previously had a 17% catastrophic + 50% no-action rate)
  outweighs the marginal disk-bloat slip. Pearl th-e182bc.

- a9ac28c: th config + th admin config: consolidate config surface, delete th api config (pearl th-9c0c34)

  Three surfaces collapsed into two:

  - **`th config`** — daily-developer surface. Gains `feature-flag <key>`
    (evaluate a flag for the active org + env; pipe-friendly stdout —
    prints just `true`/`false`/string, or `--json` for the full envelope)
    and `delete <key>` (remove a value record; `--force` required for
    secret-tier). `--env` is now a long alias for `--environment` on
    every subcommand to save a keystroke.
  - **`th admin config`** — platform-admin surface. New. Holds the
    infrequent verbs: `schemas` (list / show / create / update / delete
    / push / values), `environments` (list / create / update / delete /
    values), and `values bulk-set` + `values delete`. Same auth as
    `th config` (no `requireSuperAdmin` gate); the "admin" naming
    captures cadence + audience, not authorization level.
  - **`th api config`** — **deleted entirely**. Nobody uses `th` yet so
    no aliases needed (per user direction 2026-06-13). The old
    `th api config values` overlapping `th config get/set/list` is gone;
    the old `th api config schemas`/`environments` lives at
    `th admin config`; `th api config feature-flag` lives at
    `th config feature-flag`.

  Net: one daily surface, one admin surface, zero duplicate paths.

- d702663: Consolidate `th up` and `th vm` into a single mode story. `th up`
  now boots Smooth inside a microsandbox microVM by default — no
  Docker container, no persistent named volume, no `th vm` subcommand.
  `th up direct` is the new escape hatch for running Smooth on the
  host without a sandbox (only safe inside an already-trusted
  environment such as a CI runner or a dedicated devbox).

  The previous `th vm up` workflow (Docker container + named volume +
  host-stub credential broker) is removed entirely:

  - `th vm up`, `th vm down`, `th vm shell`, `th vm prune`, `th vm status` → gone
  - `docker/Dockerfile.smooth-vm` and `scripts/build-smooth-vm-image.sh` → deleted
  - `--sandboxed` and `--sandbox-backend` flags on `th up` → gone (sandbox is the default)

  **If you used `th vm up`, you now want `th up` instead.**

  **The persistent named volume `smooth-vm-root` is now orphaned.**
  Delete it with `docker volume rm smooth-vm-root` if you don't need
  the accumulated `~/.smooth` state from your old Docker VM.

  Outbound reachability to Docker / OrbStack / Kalima from inside the
  microsandbox VM is still supported via the existing
  `allow_host_loopback` config (which exposes `host.docker.internal`
  inside the sandbox). No nested virtualization required — Smooth
  talks to whichever container runtime is on your host over the
  network.

- 87526a0: Bump microsandbox 0.3.14 → 0.4.6 and rip out the Docker sandbox backend.

  **microsandbox 0.4.6** brings:

  - PR #673 — bounded relay handshake reads + `boot-error.json` on timeout. Failed boots now surface a structured error instead of the opaque "sandbox process exited before sending startup info" that hid the real cause on the previous 0.4.5 attempt.
  - PR #650 — `exec.log` capture + typed `ExecFailed`.
  - PR #697 — SIGKILL on `replace`-grace overruns, relevant to the bind-mount silent drop tracked in pearl th-dd0cef.

  Verified end-to-end on macOS HVF: `th up` boots the boardroom microVM, `:4400` returns HTTP 200, `th down` cleans up with no leaked `msb`/`krun` processes.

  **Docker backend removed.** `DockerSandboxClient`, `SMOOTH_SANDBOX_BACKEND=docker`, and `SMOOTH_DOCKER_BIN` are gone. Smooth has exactly two modes now — `th up` (sandboxed via microsandbox) and `th up direct` (host process, only safe in a pre-trusted environment). Docker is still callable from inside the sandbox when reaching out to host Docker / OrbStack / Colima for nested-virt-free workloads; it's just not a sandbox runtime for Smooth itself.

- 5d2039e: Three new `smooth-bench` subcommands for measuring agentic coding quality (the real `smooth-coding` decision data):

  - **`score-swe-bench --variant verified|lite`** — SWE-bench Verified / Lite (Princeton). 500 real GitHub issues from popular Python repos with held-out FAIL_TO_PASS + PASS_TO_PASS test suites. HuggingFace dataset fetch + atomic JSONL cache at `~/.smooth/bench-data/`. Score-compatible output bucketed under `"python"` for compare with the polyglot path. Industry-comparable headline number.
  - **`score-real --tasks-dir ...`** — Multi-axis benchmark on curated mini-projects in our stack (Rust + Python + TS). Each task ships a `workspace/`, hidden-tests/, and a `grade.toml` declaring weights for pass / edits / verify / tools / cost. Scorer combines them into a weighted-mean per task. First task shipped: `rust-ttl-cache` (TTL-cache wrapper around an HTTP client). Four more proposed in `tasks-real/README.md` as TODOs.
  - **`score-replay --repo owner/repo --since YYYY-MM-DD`** — Auto-harvest tasks from real merged PRs via `gh pr list --json`. For each PR ≥3 files + ≥1 test file: clone the parent commit, feed the PR title + body as prompt, score by whether the agent makes the same tests pass that the human PR did. Trait-injected (`GhCli` / `RepoFetcher` / `ReplayDriver`) so the unit tests use a `SeedFetcher` instead of real `gh` calls.

  All three reuse the existing `tui_score` dispatch (driving `th code` via tmux with the coach driver + VERIFY rule). 277 lib tests pass (86 new). Built via parallel agent workflow (`wf_31decba4-b81`) — three isolated worktrees, then merged + CLI wired by hand. Live `th code` dispatch wiring for each is left as TODO in the agents' notes — the scoring + dataset + harvest + grading infrastructure is what landed here.

- b573386: `th admin` + `th auth` (pearl th-abc4e2). New `th auth` for user identity (Supabase OAuth browser flow, M2M client_credentials, `whoami`, `logout`) stored at `~/.smooth/auth/smooai.json` — separate from the existing provider-credential management which is renamed to `th model` (Anthropic, Smoo AI Gateway, OpenRouter, OpenAI). New `th admin` for superadmin operations against `api.smoo.ai/admin/*` — currently 14 verbs across `user` (list / search / roles / magic-link) and `org` (list / show / create / members / products). All admin commands require a `th auth login` session whose account has `requireSuperAdmin` (403 otherwise). Pretty table rendering via `tabled` (heavy styling, opt-out via `--json`). Foundation for pearl th-feebd2 (`th admin onboard-customer`).
- bae137f: Two surgical bench-quality fixes triggered by the 2026-05-29 coach matrix root-cause analysis (see `docs/bench-sessions/2026-05-29-coach-vs-user.md`):

  1. **`smooth-operator`: new `AgentConfig::with_verify_tests_before_done(bool)` builder** that appends a stopgap system-prompt rule forbidding the agent from declaring done until it has run the project's test command (pytest / cargo test / npm test / go test) and seen passing tests. Targets the failure mode where deepseek/kimi/claude bail at 2-3 iterations with partial solutions (11/16, 18/20, 8/10) — the coach driver's "did you run the tests?" demand fires too late because the agent has already emitted `Completed`. This rule applies the same intent INSIDE the agent loop where it can stop early termination. Opt-in (default off) so general `th code` sessions stay snappy. Idempotent. Architectural follow-up: th-VERIFY-PHASE (full automatic test-runner invocation post-`done`).

  2. **`smooth-bench` coach persona: new BACK-OFF RULE** — if the assistant has already shown a passing test run (`N passed in X.XXs` / `test result: ok. N passed; 0 failed` / `Tests: N passed`), the coach must fire `TASK_COMPLETE` this turn instead of re-probing. Targets claude-sonnet-4-6's coach-persona regression (3/5 → 2/5) where the coach kept asking for more verification after claude had already shown passing pytest output. Should restore claude's user-persona pass rate without affecting glm-5.1's coach-driven 5/5.

- 56c5a25: bench: `--driver-persona=coach` for score-tui. The historical LLM-as-human driver is prompted as a NON-TECHNICAL end user — no shell, no file access, can't run tests, can't tell whether the agent's output is plausible. When an agent declares done with wrong output (e.g. affine-cipher decode keeping the encoder's 5-char grouping — pearl th-6a8064), the driver politely accepts and fires `TASK_COMPLETE`; the scoring phase only then runs pytest and gets FAIL. The agent never gets the feedback signal that would have let it fix the bug. New `coach` persona is a senior pair-programmer: still no tools (driver doesn't compute, doesn't run, doesn't read files), but DOES probe for an actual test run before firing `TASK_COMPLETE` and suggests concrete debugging steps without giving the answer. Default stays `user` for baseline comparability — flip via `--driver-persona=coach`. Same driver model (`smooth-summarize`) — only the system prompt + per-turn template change. Pearl th-e17b1a.
- 160eb0f: `brew install SmooAI/tools/th` — smooblue-parity install story (pearl th-e32f60). New `update-homebrew-tap` job in `release.yml` regenerates `Formula/th.rb` in [SmooAI/homebrew-tools](https://github.com/SmooAI/homebrew-tools) on every tagged release: fetches the three Unix asset tarballs, computes sha256, writes the formula with macOS arm64 + Linux x86_64 + Linux arm64 URLs, commits + pushes via SSH deploy key. Bootstrapped at v0.13.7 so the tap works today; subsequent releases will switch asset naming to `th-{macos-arm64,linux-x86_64,linux-arm64}.tar.gz` for parity with smooblue's convention. Windows target is filed as follow-up pearl th-a165b4 — needs workspace-wide Cargo feature gating (`default = ["desktop"]` / `cli-windows = []`) so the binary excludes microsandbox + ratatui on Windows.
- 19c4d00: Pearls: migrate to beads model — `.smooth/dolt/` no longer git-tracked

  Pearl `th-975dfe`. Reverses an early decision (called out explicitly in
  the prior `.gitignore` comment: "we WANT [.smooth/dolt/.dolt/]
  committed — git is how pearls sync between machines") that produced a
  recurring class of merge conflicts: Dolt rewrites the noms mutable
  pointer files (`manifest`, `journal.idx`, `*.darc`, journal-chunk) on
  every store open; git can't 3-way-merge binaries; main moving forward
  while a feature worktree was open meant the conflict-on-merge-back
  pattern recurred constantly. PR #94 (linked-worktree auto-commit
  guard) and smooai #1513 (pre-commit `git add -A` exclusion) addressed
  the worktree-as-author side but not the main-moves-forward side.

  Beads precedent: `.beads/embeddeddolt/` is gitignored; sync happens
  via dolt's custom `refs/dolt/data` ref pushed alongside normal git
  refs (`bd dolt push`/`pull`). The ref-based sync was always available
  in `th pearls`; this PR just stops materializing the on-disk noms
  files in git's tracked set.

  **Changes**:

  - `.gitignore`: add `.smooth/dolt/`. Old comment that said "we WANT
    this committed" replaced with the beads-model rationale.
  - `git rm -r --cached .smooth/dolt/`: untrack the 7 currently-tracked
    files from the index. History is preserved (history isn't rewritten);
    new commits no longer sweep noms churn into git.
  - `th pearls init`:
    - Ensures `.smooth/dolt/` is in `.gitignore` (idempotent — matches
      against `.smooth/dolt`, `/.smooth/dolt/`, `.smooth/dolt/**`).
    - On post-`git clone` bootstrap (no local store + git origin URL
      available), runs `smooth-dolt clone <origin> .smooth/dolt/` to
      populate from `refs/dolt/data`. Falls back to empty init if the
      clone fails. No manual `th pearls pull` needed.
  - `smooth-pearls`: new `dolt::clone_from(remote_url, target_dir)`
    public helper. Mirrors `recover_from_remote`'s subprocess shape but
    takes the URL as an argument instead of reading it from
    `repo_state.json` (which doesn't exist yet on a fresh bootstrap).
  - `CLAUDE.md` §5: documents the new model + implications.
  - 8 new tests covering `ensure_dolt_gitignored` (idempotency,
    wildcard variant detection, anchored leading-slash variant) and
    `read_git_origin_url` (none / present / non-git dir).

  **Other repos** (smooai, smooblue) get their own follow-up migration
  PRs (pearls `th-482e14`, `th-ad1f41`). After all three, the
  `pearls-dolt-git-conflicts` memory's "How to apply" workarounds drop
  entirely.

- 5353a1e: smooth-operator: add a `PostgresCheckpointStore` behind a new `postgres` feature (SMOODEV-1468).

  Durable, Postgres-backed implementation of the existing `CheckpointStore` trait — parity with LangGraph's `PostgresSaver`, so per-`agent_id` thread state survives process restarts. Uses an r2d2 pool of synchronous `postgres` clients (the trait is sync, mirroring `SqliteCheckpointStore`/rusqlite — not async sqlx). `connect(conn_str)` builds the pool + migrates the `checkpoints` schema; `from_pool(..)` reuses a shared app pool. SQLite/in-memory stores remain the zero-dep defaults. Covered by a testcontainers integration test that spins up a throwaway Postgres and exercises the full save/load_latest/load/list/prune + upsert + agent-scoping contract.

- c32e71c: Rename "The Boardroom" to "The Safehouse" everywhere.

  Pre-[[ADR-001]] there were multiple microVMs and "Boardroom" named the one Big Smooth + the cast lived in. After consolidation there's just one VM, and the corporate-coded name jarred against the rest of the heist/mob naming family (Big Smooth, Narc, Bootstrap Bill, Wonk, Goalie, Scribe, Smooth Operators). The Safehouse fits the metaphor: a sealed place the family runs jobs from. See `docs/Decisions/ADR-003-rename-boardroom-to-safehouse.md`.

  Code identifiers, env vars (`SMOOTH_SAFEHOUSE_MODE` / `_PORT` / `_IMAGE`), file names, tracing fields, OCI image (`ghcr.io/smooai/safehouse:latest` with entrypoint `/opt/smooth/bin/safehouse`), and docs all flip. No backwards-compat fallbacks — this is dev tooling, not a release artifact.

- 82554c4: rename the sandboxed-worker concept from "smooth-operator"/"operator" to "operative"

  Disambiguates the microVM-per-pearl sandboxed worker (which RUNS the agent
  engine) from the `smooth-operator` agent **engine** crate it consumes (being
  extracted to `smooth-operator-core`) and the public `smooth-operator`
  **service**.

  Renamed worker identifiers (engine crate `smooth-operator` / `OperatorRole` /
  all `proto/*.proto` `operator_id` wire fields are intentionally LEFT
  UNTOUCHED):

  - Runner crate/binary `crates/smooth-operator-runner` (pkg
    `smooai-smooth-operator-runner`, bin `smooth-operator-runner`) →
    `crates/smooth-operative` (pkg `smooai-smooth-operative`, bin
    `smooth-operative`). Engine dep `smooth-operator` kept as-is.
  - Container image `ghcr.io/smooai/smooth-operator` →
    `ghcr.io/smooai/smooth-operative`; `docker/Dockerfile.smooth-operator` →
    `Dockerfile.smooth-operative`; `scripts/build-smooth-operator-image.sh` →
    `build-smooth-operative-image.sh`; `scripts/build-operator-runner.sh` →
    `build-operative.sh`.
  - Env vars `SMOOTH_OPERATOR_IMAGE` → `SMOOTH_OPERATIVE_IMAGE`,
    `SMOOTH_OPERATOR_RUNNER` → `SMOOTH_OPERATIVE`,
    `SMOOTH_OPERATOR_RUNNER_NATIVE` → `SMOOTH_OPERATIVE_NATIVE`.
  - System prompt: "You are Smooth Operator…" → "You are a Smooth operative…".
  - CLI: `th operators list/kill` → `th operatives list/kill`
    (`OperatorsCommands` → `OperativesCommands`).
  - bigsmooth worker types: `OperatorClient` → `OperativeClient`,
    `OperatorRegistry` → `OperativeRegistry`, `operator_client.rs` →
    `operative_client.rs`.
  - Docs: `docs/Architecture/Operators.md` → `Operatives.md` (+ cross-links).

  The `operator_id` value/proto field name is kept (scoped value, not the
  colliding `smooth-operator` string) — no wire change. The sandbox VM name
  format moved to `smooth-operative-<id>`.

- a45fd19: SMOODEV-1409: Add top-level `th config` command with `get`, `set`,
  and `list` subcommands for day-to-day `@smooai/config` value
  management. Auths via the user JWT at
  `~/.smooth/auth/smooai-user.json` by default (with auto-refresh via
  the stored Supabase refresh_token); pass `--m2m` to use the
  service-account session at `~/.smooth/auth/smooai.json` instead.

  ```
  th config get apiUrl --environment=production
  th config set apiUrl https://api.smoo.ai --environment=production
  th config list --environment=production --json
  ```

  Org id resolves from `--org-id` flag → `SMOOAI_ORG_ID` env →
  `active_org_id` in the credentials file. The full schemas +
  environments surface still lives under `th api config` — this
  top-level command is just the muscle-memory "read or write a single
  value" wrapper that mirrors the `smooai-config` CLI's `get` / `set`
  / `list` ergonomics.

- 17b727a: SMOODEV-1793: migrate Smooth off gateway `smooth-*` slot aliases

  The Smoo AI LLM gateway is removing the `smooth-*` semantic-slot
  aliases (`smooth-coding`, `smooth-reasoning`, `smooth-reviewing`,
  `smooth-judge`, `smooth-summarize`, `smooth-fast`, `smooth-default`,
  plus deprecated `smooth-planning` / `smooth-thinking` and the various
  `smooth-<slot>-<vendor>` sub-aliases). After cutover, any request for
  those model names returns HTTP 400 `Invalid model name` from the
  gateway.

  What changes:

  - **New mapping table** in `smooth_policy::smooth_alias` is the single
    source of truth for legacy → concrete model rewrites:

    | Old slot                                 | Concrete model_name     |
    | ---------------------------------------- | ----------------------- |
    | `smooth-coding` / `smooth-default`       | `deepseek-v4-flash`     |
    | `smooth-reasoning` (+ planning/thinking) | `deepseek-v4-pro`       |
    | `smooth-reviewing`                       | `minimax-m2.7-direct`   |
    | `smooth-judge` / `smooth-summarize`      | `gemini-2.5-flash`      |
    | `smooth-fast`                            | `gemini-2.5-flash-lite` |

  - **Migration shim** in `smooth_cast::provider_migration` walks every
    routing slot on a loaded `ProviderRegistry` and rewrites legacy
    aliases in place. `load_providers_with_migration(path)` is a drop-in
    replacement for `ProviderRegistry::load_from_file` that loads,
    migrates, **saves the file back if anything changed**, and emits one
    `tracing::info!` per rewrite so users see the migration once.

  - **Every `providers.json` loader** in the workspace funnels through
    the migration loader (smooth-cli, smooth-bigsmooth, smooth-code,
    smooth-bench, smooth-operative — 31 call sites total). Existing
    users' on-disk configs are rewritten on first load; the in-memory
    migration also covers routing JSON shipped to operatives so older
    Big Smooth builds can still drive a freshly-built operative.

  - **The TUI model picker** drops the hardcoded `SMOOTH_ALIASES` array
    and now offers the concrete catalog defaults. The picker also
    surfaces metadata (use-case tags, tier, cost, benchmark) sourced
    from the gateway's `/v1/model/info` schema (offline fallback
    catalog colocated in `smooth-code/src/model_picker.rs`).

  - **`th model login`** no longer offers the dead `smooth-*` aliases
    for the SmooAI Gateway provider.

  Coordination: the SmooAI-side gateway change (LiteLLM config) can roll
  out once this branch lands on Smooth `main`, is rebuilt, and reinstalled
  via `pnpm install:th`.

- 9267296: smooth-operator: add an `LlmProvider` trait + `MockLlmClient` test harness (SMOODEV-1467).

  `LlmProvider` abstracts the LLM call (`chat` + `chat_stream`); the real `LlmClient` implements it by delegating to its inherent methods. `MockLlmClient` is a deterministic, scriptable test double (text / tool-call / error / streaming-event responses) that records every request for assertions and is cheap to clone (shared state). This is Phase 0 of the LangGraph-parity work (epic SMOODEV-1466) — the seam every later phase (durable checkpointing, HITL pause/resume, persistent memory, vector RAG, structured output, OTel gen_ai spans) is unit-tested against. 10 unit tests + a doctest; clippy/fmt clean.

- 80b6fbc: LiteLLM prompt-caching client support. The operator-runner now sends
  Anthropic-shaped `cache_control: {type: ephemeral}` markers on Claude
  routes (model id contains `claude` / `sonnet` / `opus` / `haiku`, or one
  of the Smooth LiteLLM aliases like `smooth-coding-claude`) when the
  api_base looks like LiteLLM or anthropic.\*. We mark three breakpoints:
  the system prompt, the last tool definition (caches the entire tool
  block plus system), and the last message in history (extends the cache
  turn-by-turn). Non-Claude / OpenAI / Gemini routes still send a plain
  string `content` — no cache_control on the wire.

  Cache-hit numbers (`usage.prompt_tokens_details.cached_tokens`) are
  read back from the response, aggregated in `CostTracker.
total_cached_tokens`, plumbed through `AgentEvent::Completed.
cached_tokens`, and surfaced on Big Smooth's `[METRICS]` pearl-comment
  line so a session's cache-hit ratio is observable. Requires the smooai
  LiteLLM gateway to have `cache_control_injection_points` configured —
  without that, this code is a no-op.

- 2f903ee: Two TUI polish pearls landed together (`th-91d8af` + `th-a10c2d`):

  **Pearl th-91d8af — bare `th` shows a friendly explainer.**
  Running `th` with no subcommand used to drop new users straight
  into the smooth-code TUI cold. Now it prints a one-screen
  explainer covering what `th` is for, what the main subcommand
  families do (`th code`, `th up`, `th pearls`, `th api`,
  `th cast`, `th mcp`), and the most useful starter commands.
  `th code` (and the existing top-level shortcuts `th --resume`,
  `th --list`, `th --agent <name>` from pearl
  `th-resume-top-level`) continue to launch the TUI directly —
  the explainer only triggers when no subcommand and no code-mode
  flags are present.

  **Pearl th-a10c2d — TUI shows the upstream model behind a smooth-\* alias.**
  When the user routes through an alias like `smooth-coding`, the
  gateway resolves it to a concrete upstream (e.g.
  `qwen3-coder-flash`). Previously the TUI only ever showed the
  alias. The agent loop now captures the `model` field from chat
  completion / Anthropic responses (and from streaming chunks)
  into a new `LlmResponse.resolved_model` field, emits a one-shot
  `AgentEvent::ModelResolved { alias, upstream }` per session when
  the alias differs (idempotent — only re-emits if the upstream
  changes mid-run), and the smooth-code status bar renders
  `smooth-coding → qwen3-coder-flash`. Concrete-model selections
  where alias == upstream stay quiet so the status bar doesn't
  clutter.

  Both behaviours are forward-compatible: the new
  `AgentEvent::ModelResolved` variant slots into the existing
  `#[serde(tag = "type")]` enum, so old clients silently skip it
  and new clients connected to old runners just don't see it.

- 864e834: runner: add `todo_list` tool for cross-turn task state (opencode parity)

  Adds a `todo_list` tool to smooth-operator-runner. Operates on a small
  JSON file at `.smooth/todos.json` with four actions:
  `add` / `list` / `update` / `clear`. Persists across the runner's
  fresh-per-turn process boundary so on turn 2 the agent can
  `todo_list action='list'` to find what it was doing — the structural
  anchor opencode uses and smooth was missing.

  Pearl `th-1d6699`. Diagnosed by side-by-side pane capture of opencode
  vs smooth on `cleanup-node-modules-orphans`: opencode emits a
  `# Todos` checkbox list as part of its plan, marks items in_progress
  as it executes, and on `"yes, proceed"` reads the pending todo and
  issues ONE concrete `rm -rf <paths>` command. Smooth had no equivalent
  tool — every other registered tool (read_file, write_file, edit_file,
  apply_patch, list_files, grep, lsp, bash, bg_run, http_fetch,
  project_inspect, read_memory, write_memory) is single-shot or
  project-scoped, none track per-task state.

  Wired through:

  - `crates/smooth-operator-runner/src/main.rs` — `TodoListTool` impl
    - `TodoStore` (JSON-file-backed, atomic rename-from-tmp write) +
      8 unit tests including cross-process persistence.
  - `crates/smooth-bigsmooth/src/policy.rs` — added `todo_list` to both
    `registered_tool_names()` and `read_only_tool_names()`. Without
    this entry Wonk denies every call and the agent logs the
    "I cannot use the todo_list tool" excuse.
  - `crates/smooth-operator/src/cast/prompts/fixer.txt` — new section
    teaching the agent the planning → executing → completion lifecycle
    for the tool. Anchored on "call `list` at the start of every
    continuation turn — it tells you what was already done and what's
    next."

  Bench impact at `deepseek-v4-flash`: not measurable — the weak model
  hallucinates "tool not in allowlist" rather than calling it (no
  allowlist gate exists in direct mode; the LLM is making up an
  excuse). The tool is structurally in place for stronger models
  (v4-pro, claude-sonnet) where the multi-turn discipline pays off.
  Filed as architectural parity, not a single-fixture lift.

- e8fb9e4: WebSearch tool (Exa MCP primary, Parallel fallback) + Wonk allowlist + score-research bench dimension

  **New tool — `web_search`** (pearl th-2cc3f1). Mirrors OpenCode's
  `tool/websearch.ts` + `tool/mcp-websearch.ts`: posts an MCP JSON-RPC
  `tools/call` to a hosted LLM-tuned search provider that returns
  extracted, LLM-ready text (no separate fetch step needed for the
  snippets). Two providers behind the same surface so smooth-vs-opencode
  head-to-head benches stay on the same backend:

  - **Exa** (`mcp.exa.ai`) — primary. Tool name `web_search_exa`, knobs
    `type` / `numResults` / `livecrawl` / `contextMaxCharacters`.
  - **Parallel** (`search.parallel.ai`) — fallback. Tool name `web_search`.

  Provider picked from `SMOOTH_EXA_API_KEY` / `SMOOTH_PARALLEL_API_KEY`;
  `SMOOTH_WEBSEARCH_PROVIDER=exa|parallel` overrides. Registers only when
  a provider key is configured — otherwise the tool would always error
  on first call and just clutter the schema.

  **Wonk policy** (pearl th-bf3f6e). Adds `mcp.exa.ai` + `search.parallel.ai`
  to `phase_network_defaults()` baseline. Single-purpose, easy to audit,
  no wildcards.

  **`score-research` bench dimension** (pearl th-f4ac64). Sibling to
  `score-cleanup`. Grades the agent's ability to answer questions that
  REQUIRE web search — fact lookups, identifying a title from a fuzzy
  description, etc. Two axes: `answer_correctness` (case-insensitive
  keyword matching, `min_correctness` default 1.0 hard-kills below
  threshold) and `cited_source` (URL detection — anti-hallucination
  probe). First fixture `research-hijack-year` probes the chain
  end-to-end (find "Hijack" series → year + service → cite).

  Reuses `CoachCfg` + `AgentDriver` trait so mock/opencode/smooth/pi
  all work out of the box. Mock agent `perfect-research-hijack.sh`
  makes the pipeline runnable in CI without API spend.

### Patch Changes

- aed48d2: th: unify active-org resolution across `th api`, `th config`, `th auth`

  `th api orgs switch <id>` wrote the active org only to the legacy
  `smooth-api-client` store at `~/.smooth/auth/smooai.json`, but
  `th config list` (and any other subcommand that uses
  `smooai-client-shared`'s `default_user()` store) read from a different
  file (`~/.smooth/auth/smooai-user.json`). Net effect: switch reported
  success, then `th config list` immediately failed with
  "no active org set — pass `--org-id <id>`, set SMOOAI_ORG_ID, or run
  `th api orgs switch <id>`" — the same command the user just ran.

  Adds a shared `crate::active_org` module with two functions:

  - `resolve(override_org)` — consults `--org` flag → `$SMOOAI_ORG_ID` →
    every credential store on disk (legacy api-client + client-shared
    M2M + client-shared User), returning the first non-empty
    `active_org_id`.
  - `set(org_id)` — fans the write out to every credential store whose
    file already exists. Won't fabricate a stub User session for an
    M2M-only user.

  Wires `th api orgs switch`, `th api orgs show`, the `th api`
  `require_active_org` helper, and `th config`'s `resolve_org` through
  the shared module. Covered by ten new cross-subcommand contract
  tests in `crates/smooth-cli/src/active_org.rs`.

- a740746: test: make `auth::active_org` tests hermetic so they stop flaking the Release run

  `auth::active_org`'s tests pointed `default_user()` at a tempfile by mutating
  the process-global `SMOOAI_USER_AUTH_FILE` env var under a module-local
  `ENV_LOCK`. The cross-store `active_org` module's tests mutate the _same_ env
  vars under a _separate_ lock, so the two modules raced when cargo ran them in
  parallel in the `th` test binary: one clobbered the other's env mid-test, the
  read hit the wrong file, the assert failed, and the failure poisoned the mutex
  — cascading. It passed in PR Checks but lost the race in the Release
  (Changesets) workflow, keeping Release red (and blocking changeset versioning).

  Fix: drop the env entirely. `set`/`resolve` now delegate to private
  `set_in(&store, …)` / `resolve_in(&store)`, and the tests construct
  `CredentialsStore::at(<tempfile>)` directly — no global env, no lock, no
  cross-module race. Verified passing under `--test-threads=16`. Pearl th-2944e5.

- 0dfa72b: SMOODEV-1787 (PR 1/4, dual-engine collapse): consume the published
  `smooai-smooth-operator-core` engine instead of the in-tree copy, and
  delete the in-tree `crates/smooth-operator/`.

  The in-tree engine and the public `smooth-operator-core` were the same
  engine but had diverged. The only differences were (a) the public core
  gates its BigSmooth control-plane reporter behind a `bigsmooth` cargo
  feature (with a no-op stub when disabled) and (b) cosmetic
  public-sanitization edits (doc rewording, neutralized example hosts in
  tests, `smooth_operator` → `smooth_operator_core` in doc examples, a
  provider-agnostic `ModelRouting::default()`, a redacting `Debug` for
  `ProviderConfig`/`LlmConfig`, and a wider retry-status set). smooth never
  enables the `bigsmooth` feature and never sets a reporter, so the gated
  reporter calls were dormant no-ops — the cutover loses nothing.

  Wiring: the workspace dep KEY stays `smooth-operator` and is package-aliased
  to `smooai-smooth-operator-core`, so all ~12 consumers' `use smooth_operator::…`
  imports compile unchanged. Pinned as a rev-locked git dep (not a sibling
  path dep) to avoid the CI `cargo metadata` failure that SMOODEV-1464 hit
  with a `../`-style path dep. No functional change, no module removal — that
  lands in later PRs.

- c6cca91: Cut smooth over to the published `smooai-smooth-operator-core` v0.14.0 (crates.io); re-home the th-code harness into smooth's own crates

  This is the final PR of the engine-decouple program (SMOODEV-1790, PR 4/4). The
  engine `smooai-smooth-operator-core` is now published on crates.io at `0.14.0` —
  a clean, GENERIC agent engine with the `th code` coding harness REMOVED.
  Previously smooth depended on the engine via a git rev (`bb9a256`) that still
  carried the harness, which is why it kept building.

  - **Engine dep switched to crates.io 0.14.0.** Root `Cargo.toml`:
    `smooth-operator = { git = …, rev = "bb9a256…" }` →
    `smooth-operator = { version = "0.14.0", package = "smooai-smooth-operator-core" }`.
    The dep KEY stays `smooth-operator` so the `use smooth_operator::…` imports for
    the generic engine API are unchanged. `Cargo.lock` now resolves the engine from
    `registry+https://github.com/rust-lang/crates.io-index` (checksum-pinned), not a
    git source — the git-rev bridge is gone.

  - **New `smooth-cast` crate** re-homes the bits the engine dropped, built on the
    engine's generic public API (`Agent`/`ProviderRegistry`/`ToolRegistry`/generic
    `Cast`/`OperatorRole`/`Clearance`):

    - `coding_workflow` — the `th code` single-agent outer loop
      (`run_coding_workflow`, `task_text_has_cleanup_intent`, …).
    - `skills` — skill discovery (`discover`, `SkillScope`, `SkillSource`, `Skill`)
      plus the built-in `create-skill` skill.
    - `cast` — the four coding-harness cast roles the generic engine no longer ships
      (`fixer`, `oracle`, `chief`, `intent_classifier`), and a `cast::builtin()` that
      returns them on top of the engine's generic built-in roles. All moved tests came
      with the code.

  - **Consumers repointed** to `smooth-cast`: `smooth-operative` (coding_workflow +
    `fixer` role resolution), `smooth-code` (skills + `chief`/`intent_classifier`
    routing), `smooth-cli` (skills + `--agent` role resolution), `smooth-bigsmooth`
    (skills + session auto-naming). Every site that did `Cast::builtin().get("fixer"|
"oracle"|"chief"|"intent_classifier")` now uses `smooth_cast::cast::builtin()`.

  - The Big-Smooth reporter hooks the engine also dropped stay deleted — verified
    zero smooth consumers (`with_reporter`/`BigSmoothReporter`/`ReporterEvent`/
    `report_to_bigsmooth`/the `bigsmooth` engine feature). smooth's own
    `smooth-bigsmooth` gRPC crate is unrelated and untouched.

- a7ba717: release: align all changeset package names to `@smooai/smooth`

  `pnpm changeset version` (the Release workflow's Version Update step) was
  failing repo-wide with "Found changeset … for package X which is not in the
  workspace": 35 accumulated changeset files declared the package under three
  wrong spellings — `smooai-smooth` (21), bare `smooth` (12), and
  `@smooai/smooth-cli` (2) — none of which exist in the workspace. The only
  package is the root `@smooai/smooth` (per `package.json`). Renamed every
  changeset to `@smooai/smooth` so versioning can run and the backlog of
  pending changesets (including the pearls auto-heal + test-hygiene fixes)
  finally gets a version bump + changelog entry. Pearl th-645e54.

- 74890e8: bench: stop `current_commit_sha()` returning empty under the pre-push hook / CI

  `current_commit_sha()` shelled out to `git rev-parse HEAD` while inheriting
  the caller's git environment. Under the git pre-push hook (and some CI
  checkouts) `GIT_DIR` / `GIT_INDEX_FILE` / `GIT_WORK_TREE` / `GIT_PREFIX` /
  `GIT_COMMON_DIR` are exported, which made the child git print nothing (exit 0,
  empty stdout) instead of the real sha. That empty string failed the
  `current_commit_sha_returns_something_non_empty` test in the full `cargo test`
  run — blocking every direct push (pre-push hook) and the Release (Changesets)
  workflow, while passing in isolation and in PR Checks.

  Fix: strip the inherited `GIT_*` vars before invoking git so it rediscovers the
  repo from cwd, and treat empty stdout as the `"unknown"` sentinel (same as the
  git-failure path) so the function never returns `""` — which also stops release
  Scores from being tagged with an empty provenance string. Pearl th-e2cbc9.

- d9d2422: release: install `protoc` on macOS runners in the binary-build matrix

  The Release workflow's cross-platform binary-build matrix installed
  `protobuf-compiler` only on Linux (`if: runner.os == 'Linux'`), so the
  `aarch64-apple-darwin` / `x86_64-apple-darwin` targets had no `protoc` and
  `smooai-smooth-narc`'s prost `build.rs` failed with "Could not find protoc".
  This job had never run to completion before (the Version Update step always
  failed first on the changeset package-name bug, th-645e54), so the gap was
  never exposed. Added a macOS-gated `brew install protobuf` step. Pearl
  th-14bddf.

- ae10d6b: fixer prompt: add explicit "When the user confirms: EXECUTE" rule

  When the prior assistant turn enumerated a destructive plan ending in
  "Proceed?" and the user's next message is "yes" / "proceed" / "go" /
  "do it" etc., the agent must invoke the destructive command directly,
  not re-enumerate or re-ask for confirmation, and not pivot to a
  different task.

  Lifts `cleanup-node-modules-orphans` pass rate from 0/5 to 3/5 under
  strict-coach mode (minimal "yes, proceed" reply). The old prompt
  implied the meaning of "yes" but never explicitly told the agent what
  behavior to perform on receipt — the model was free to interpret
  "yes" as a context-restate cue, which the bench's idle detector then
  mistook for a fresh first-idle and pasted the coach reply again,
  producing the score-0.55 zero-bytes-freed failure shape.

  Pearl: th-e182bc (re-scoped — was misdiagnosed as inter-turn context
  loss; instrumentation confirmed prior_messages flow is intact through
  all 3 hops, the failure is in agent action policy)

- f5abcd3: fixer.txt: revert todo_list teaching section (regressed v4-pro 3/3 → 0/3)

  The "Multi-turn tasks: use `todo_list`" section added in
  th-1d6699's commit hurt every model tier tested:

  - deepseek-v4-pro: 3/3 perfect → 1/3 partial (0.8) + 2/3 must_preserve
    violations (0.35)
  - deepseek-v4-flash: agent hallucinated "tool not in allowlist"
    excuses, didn't actually call the tool

  Post-revert v4-pro is back to 3/3 perfect (3,559,751 / 3,559,751 /
  3,557,724 bytes freed). The TodoListTool itself stays — it's
  architecturally correct and ready for stronger models to pick up
  organically. The prompt-injection approach was too prescriptive
  and conflicted with the existing destructive-plan discipline. Pearl
  th-1d6699 remains in_progress for a re-attempt that demonstrates
  the tool via a concrete example rather than a 24-line procedural
  sermon.

- 8e21cf1: model defaults: judge → groq-llama-3.3-70b, fast → groq-llama-3.1-8b (pearl th-3468bd)

  Post SMOODEV-1793 the concrete slot defaults in `SmoothSlot::concrete_default`
  routed `judge` to `gemini-2.5-flash` and `fast` to `gemini-2.5-flash-lite`.
  Update both to Groq Llama models matching the gateway's previous
  `smooth-*` primaries:

  - **`fast`** → `groq-llama-3.1-8b`. Sub-300ms first token, ~10× cheaper than
    Gemini Flash Lite. Matches the gateway's old `smooth-fast` primary
    (Groq Llama 3.1-8B-Instant).
  - **`judge`** → `groq-llama-3.3-70b`. An 8B is too small for adversarial
    prompt-injection detection — the 70B catches paraphrase attacks the
    8B misses, while still landing under 1s on Groq and well under
    Gemini Flash on cost. Judge gates tool execution; refusal quality
    beats latency at this slot.
  - `summarize` stays on `gemini-2.5-flash` — its 1M context window is the
    load-bearing feature for context compaction.
  - Coding / reasoning / reviewing / default unchanged.

  Catalog entries for both Groq models added to `fallback_catalog()` so
  the picker has metadata (use_cases, tier, cost, description, AA index)
  in offline mode. Migration and policy tests updated.

- 4b186d4: install:th now drops smooth-dolt and native smooth-operative into ~/.cargo/bin (pearl th-92dac3)

  `th code` invoked from outside the smooth repo (e.g. `~/dev/smooai/smooai/`)
  warned "smooth-dolt binary not found" and hard-errored on the
  first dispatch with "native smooth-operative not found". Root cause
  in both: the discovery code walks from `CARGO_MANIFEST_DIR` and the
  process cwd looking for `target/release/<binary>` — neither finds
  the binary when the cwd is a different repo.

  Fix: `pnpm install:th` now also:

  - Runs `cargo install --path crates/smooth-operative --force`, which
    drops the native binary at `$CARGO_INSTALL_ROOT/bin/smooth-operative`
    (typically `~/.cargo/bin/`) alongside `th`.
  - Runs `scripts/install-smooth-dolt-to-cargo-bin.sh`, which copies
    `target/release/smooth-dolt` to the same dir. The copy is skipped
    when the destination is already byte-identical (cheap hot
    reinstalls; safe against a running `th up`).
  - `find_native_operative_binary()` in `smooth-bigsmooth` is
    extended with a `$CARGO_INSTALL_ROOT/bin` → `$CARGO_HOME/bin` →
    `~/.cargo/bin` fallback, refactored into a pure helper
    (`cargo_bin_native_operative`) with 4 unit tests covering all
    three precedence rungs and the missing-binary case.

  `install:th:full` carries the same install steps (and bumps the
  unconditional smooth-dolt rebuild). `th code` from anywhere now
  finds both binaries without further setup.

- f9d751e: install:th now builds smooth-dolt automatically (pearl th-a49716)

  Previously `pnpm install:th` ran `build:web` + `build:runner` + `cargo
install`, but never built the `smooth-dolt` Go binary that
  `th pearls` needs. Fresh installs (and post-rebase ones) hit the
  "⚠ smooth-dolt binary not found. Pearl sync may not work." warning
  on every `th code` launch and the user had to read the warning and
  run `scripts/build-smooth-dolt.sh` by hand.

  Now:

  - `pnpm install:th` — adds `build:smooth-dolt:if-stale`. The build
    script accepts a new `--if-stale` flag that skips the Go build
    entirely when `target/release/smooth-dolt` already exists AND
    every `*.go` / `go.mod` / `go.sum` under `go/smooth-dolt/` is
    older than the binary. Hot installs pay zero cost; cold installs
    and source bumps trigger a real build.
  - `pnpm install:th:full` — NEW. Same shape as `install:th` but
    invokes `build:smooth-dolt` unconditionally. Use after a Go
    toolchain change, a Dolt upstream bump, or when you suspect a
    stale binary.

  No behavior change to the standalone `bash scripts/build-smooth-dolt.sh`
  invocation — without `--if-stale` it always builds, same as before.

- ab5a455: th pearls: skip the git auto-commit of pearl state when run from a linked worktree

  `th pearls` mutations auto-commit the `.smooth/dolt/` store to git so pearl
  state syncs across machines. Dolt rewrites its mutable pointer files
  (`journal.idx`, `manifest`, the journal chunk) on every store open, and each
  linked worktree checks out its own copy — so committing those onto a feature
  branch produced binary pointer divergence that couldn't be merged back to
  main (recurring `.smooth/dolt` conflicts).

  `auto_commit_pearl_state` now detects a linked worktree (`git rev-parse
--git-dir` ≠ `--git-common-dir`) and skips the git commit there, logging a
  hint to run pearl mutations from the primary worktree. The dolt mutation and
  `th pearls push` (refs/dolt/data) still capture the change, so nothing is
  lost — pearl state simply stays on one lineage. Primary-worktree behaviour is
  unchanged.

- d937223: Pearl th-01c714: stop multi-line task prompts from fragmenting into
  N `You:` submissions in the `smooth-code` TUI when driven by the
  bench harness.

  After pearl th-7fdfa9 fixed the `j`-for-newline bug in
  `tmux_driver::send`, newlines now correctly survive into the TUI's
  input box — but the TUI's input handler treats every `\n` as Enter
  (submit). So a 13-line task prompt arrived as 13 separate `You:`
  submissions instead of one, fragmenting the conversation. Evidence:
  `~/.smooth/bench-runs/80c092b0/python-affine-cipher.pane.log`.

  Two-pronged fix (belt-and-suspenders):

  1. **Bracketed paste in `tmux_driver::send`.** Added `-p` to the
     `tmux paste-buffer` invocation so the content is wrapped in
     `\e[200~ ... \e[201~` markers. Bracketed-paste-aware TUIs use
     these markers to keep embedded newlines as soft newlines rather
     than treating each as Enter. If the receiving application has not
     enabled `\e[?2004h`, tmux strips the markers and behaviour is
     identical to the prior non-`-p` path — so `-p` is a safe upgrade.

  2. **Flatten multi-line prompts before sending.** Reformatted
     `lib::build_prompt` to produce a single line (semicolon-joined
     clauses). Added `human_driver::flatten_for_tui` which collapses
     newlines to `|` and is applied to the initial task prompt seed
     and every driver-model follow-up before `driver.send`. Even if
     the TUI never honors bracketed paste, the flattened form is
     guaranteed to land as one `You:` block. Cheap insurance against
     future TUI input-handler changes.

  Tests added:

  - `lib::build_prompt_is_single_line` — asserts no `\n`/`\r` in the
    bench task prompt.
  - `human_driver::flatten_for_tui_*` (5 cases) — covers passthrough,
    trimming, empty input, blank-line dropping, and the multi-line
    → pipe-separated transformation.

  Verified with `score-tui --pr --task-limit 1 --debug` against a
  running Big Smooth: the new pane log shows the seeded prompt
  landing as a single `You:` block containing the full text, instead
  of one `You:` block per line as before. The task itself still
  fails (affine-cipher under single-shot constraints is hard), but
  the harness now sends what we intend it to send.

- 73e0748: bench: deterministic test-result regex scorer + forensic dump. Adds `parse_native_test_summary` with per-language parsers (cargo's `test result:`, pytest's `N passed, N failed`, jest summary) that run **before** the LLM judge — the judge gets a 4 KB trimmed window and routinely returns 0/0/0 when the canonical summary line falls outside that window, scoring real passes as FAIL. Verified on rust-acronym across all 4 models in the last matrix: saved `src/lib.rs` passed 10/10 against `cargo test` on the host but scored FAIL. Also writes `~/.smooth/bench-runs/<id>/<task>/.smooth-score-forensic/{combined.txt,summary.json}` on every score attempt so failures are forensically diagnosable. Pearl th-086f0f.
- a9668dc: Pearl th-2b5f63: add `th cast models` — list live model groups
  exposed by the configured LiteLLM provider (e.g. llm.smoo.ai) via
  `GET /v1/models`.

  Useful for confirming deploys, debugging routing, and copying
  alias names. The default provider is the one backing the `default`
  routing slot (what `th routing show` highlights); pass
  `--provider NAME` to override on multi-provider setups.

  Flags:

  - `--provider NAME` — override the provider id (default: the
    provider backing the `default` routing slot).
  - `--filter PATTERN` — case-insensitive substring filter on
    model ids.
  - `--json` — emit `{"data":[{"id":"..."}]}` for scripting.

  The parser is tolerant of LiteLLM responses with embedded ASCII
  control bytes (we strip 0x00-0x1F before strict JSON parsing) and
  of truncated responses (a byte-scan fallback recovers any
  complete `"id":"NAME"` entries). When the strict and lossy
  counts disagree, the footer surfaces a `!` warning so deploys
  returning partial bodies don't fail silently.

  Exits 2 if no provider is configured (`run th auth login`), and
  prints the status code + first 200 chars of the body when the
  provider responds non-200.

- 20a76be: Wire `AgentConfig::with_verify_tests_before_done` into the operator-runner dispatch path (pearl th-393aed). The builder landed earlier today but no caller was using it, so the "no final response until tests pass" rule wasn't actually firing in any bench run. Now: `smooth-operator-runner` reads `SMOOTH_VERIFY_TESTS` from its env and calls the builder with the parsed boolean (`1` / `true` → on, anything else / unset → off). Big Smooth's per-task operator-runner spawn (`server.rs`'s minimal env*clear + whitelist) now also passes the var through, alongside the other `SMOOTH_WORKFLOW*\*`knobs. Default off — general`th code`sessions still see no behavior change; bench runs flip it on by booting Big Smooth with`SMOOTH_VERIFY_TESTS=1 th up direct …` so all in-bench operator runs see the rule.
- 9800b16: Pearl th-399196: `smooth-bench score-tui` — drive `th code` via tmux + LLM-as-human loop.

  Adds a new `smooth-bench score-tui` subcommand that runs the curated
  aider-polyglot sweep against the real `th code` TUI instead of the
  WebSocket chat-agent path, so the bench exercises what a human user
  actually touches: the TUI's prompt parsing, the model alias→upstream
  display, tool-call surfacing, and session lifecycle.

  How it works:

  - A new `TmuxDriver` (`crates/smooth-bench/src/tmux_driver.rs`)
    spawns `th code` inside a detached tmux session, types into it
    via `send-keys`, and reads visible output via `capture-pane`.
  - A new LLM-as-human loop (`crates/smooth-bench/src/human_driver.rs`)
    asks a cheap driver model (default `Activity::Summarize`) to play
    the role of a user testing the assistant: it reads the current
    pane snapshot each turn and decides what to type next, or fires
    the `TASK_COMPLETE` / `TASK_STUCK` sentinels.
  - The new orchestrator (`crates/smooth-bench/src/tui_score.rs`)
    ties it together: per task it preps the scratch dir via the
    newly-extracted `prepare_task` helper, drives the human loop,
    then scores via the shared `finalize_and_score` helper.
  - Emits the same `Score` shape as `score --pr` / `score --release`,
    plus a `via: "tui"` marker on the `TuiSweepRun` for downstream
    analysis.

  Flag surface mirrors `score`: `--pr`, `--release`, `--budget-usd`,
  `--output`, `--url`. New TUI-specific flags: `--tmux-session`
  (default `smooth-bench-tui`), `--th-binary` (default `th`),
  `--driver-model` (default `summarize`), `--max-turns` (default 15),
  `--task-timeout-s` (default 900).

  The existing WebSocket `score` path is unchanged — `chat_driver.rs`
  and `sweep.rs` are untouched aside from shared helpers extracted up
  into `lib.rs` (`prepare_task`, `finalize_and_score`) which the
  WebSocket path now also uses for zero-drift task setup.

  Tests:

  - `tmux_driver` exercised against `echo`, `cat`, and `sleep` shell
    fixtures (no `th` needed for unit tests).
  - `human_driver` decision parsing + prompt assembly tested with a
    hand-rolled `FakeDriver` (no live LLM).
  - `tui_score` aggregation + shell-escape + tool-call counting
    tested without spawning real microVMs.

  Heavier integration tests against a real Safehouse + dataset are
  left to the operator to invoke via `smooth-bench score-tui --pr`.

- 35b2cb1: Pearl th-3c0b13: route every CLI "Smooth" / "Smoo AI" through the
  gradient wordmark helpers.

  The `crates/smooth-cli/src/gradient.rs` helpers (`smooth()`,
  `smoo_ai()`) already existed but were only used in a handful of
  places. Every other user-facing `println!` printed the brand name
  as plain bold/colored text, so the same word read three different
  ways depending on which command was running. This patch swaps the
  literal "Smooth" / "Smoo AI" / "Big Smooth" / "Smoo AI Gateway" /
  "Smoo AI platform" / "Smooth Operators" / "Smooth home" mentions
  in `th`'s console output for calls to the existing gradient helpers
  so the wordmark renders consistently with the logo (Smoo
  orange→pink, th teal→blue).

  Touches the bare-`th` explainer, `th up`, `th down`, `th status`,
  `th auth status`, `th auth login` picker, `th operators`, `th
inbox`, `th doctor`, and `boot_ui.rs`'s `✻ Smooth booting` header.
  Status / auth columns lose their `{:<N}` width formatting (which
  would have been confused by the ANSI escapes) in favour of hand-
  padded spacing so the visible columns still line up.

  Tracing logs, error messages, identifier names, doc strings, and
  the systemd unit file's `Description=` line are deliberately left
  plain — those either land in log files or get piped/grep'd and
  shouldn't carry ANSI.

- d38e87c: `th pearls` auto-doctor for the orphaned-`smooth-dolt serve` lock wedge. Repro shape: an earlier `th up` spawned `smooth-dolt serve <data-dir> --socket /tmp/smooth-dolt-shared/<hash>.sock` as a child. Parent died (`th down`, crash, agent worktree teardown), the serve child got reparented to init, and the socket file was cleaned up — leaving the serve process running with no way to reach it but still holding the `noms/LOCK` file. `try_attach_handle` (does `socket.exists()`) returns None, `SmoothDolt::new` falls back to CLI mode, `smooth-dolt exec` tries to grab the lock, fails with `Error 1105: cannot update manifest: database is read only`. Every `th pearls create / update / close / commit` wedges until the user manually kills the orphan. Now: on the read-only error, `run_cli` invokes `auto_doctor_clear_orphan_server` which uses `lsof -t` to find LOCK file holders, verifies via `ps -o command=` that each holder is actually `smooth-dolt serve` (refuses to kill debuggers / backup tools / IDEs that happened to open the file), `SIGTERM`s the orphan, waits 500 ms for the kernel to release the lock, retries once. Best-effort: failures inside the doctor itself fall through to the original error rather than masking it. Pearl th-49e37b.
- 1ead328: docs: add `docs/Engineering/Using-th-CLI.md` covering the full `th api` / `th admin` (planned) / `auth.smoo.ai` OAuth2 client_credentials flow, plus a `.claude/hooks/th-curl-hint.sh` PreToolUse hook that nudges Bash commands toward `th` whenever they're about to raw-curl `api.smoo.ai`, `auth.smoo.ai/token`, or `atlassian.net/rest/api`. Hook also covers the `gh secret set --body -` newline footgun (SMOODEV-879) and raw `pnpm sst secret list` leakage (SMOODEV-908). Mirrored in the smooai monorepo so the same hints fire in both repos. Pearl th-500495.
- b826a0d: Pearl th-7840d8: animated boot UX for `th` cold start + `th up`.

  Replaces the bare `Starting Smooth...` line (and the silent gap
  during `th up`'s daemon spawn) with a per-step indicatif spinner
  cascade so the user can see what's happening while the Safehouse
  microVM and the in-VM cast services come up.

  Steps shown in both entry points:

  ```text
  ✻ Smooth booting
      ✓ starting Safehouse microVM
      ✓ cast online (wonk · goalie · narc · scribe · archivist · diver · groove)
      ✓ operator-runner pool warm
      ✓ health check
  ```

  Spinners turn into a green `✓` on success or a red `✗ — <reason>`
  on timeout / failure. The boot transcript stays in the terminal
  after `th up` returns. v1 drives the steps off observable TCP +
  HTTP probes against `localhost:4400`; no daemon-side IPC needed.

  New module `crates/smooth-cli/src/boot_ui.rs` with a tested
  `BootIndicator` / `BootStep` state machine. Adds `indicatif` to
  the workspace deps.

- 23eb7df: Pearl th-7b95ef: stop operator-runner stderr from being persisted as assistant chat content. The runner's diagnostic output now goes only through `tracing` to stderr (debug for repeated bootstrap noise, info for actionable events); bigsmooth's stdout reader now classifies lines via a new `classify_runner_stdout_line` helper and drops anything that isn't valid JSON. Both stdout-non-JSON and stderr forwarding to `ServerEvent::TokenDelta` are removed, so session JSONs are no longer poisoned with `[runner] SMOOTH_POLICY_FILE env var not set` blobs. New regression test asserts the runner binary's stdout contains zero `[runner]` substrings.
- 484ee20: Pearl th-7fdfa9: fix two harness bugs in `smooth-bench score-tui`
  that produced the same false-pass smell as th-f46efa.

  1. **`tmux send-keys -l` mangles newlines into `j`.** Every `\n` in a
     multi-line task prompt was being interpreted as the `C-j` keysym
     and, in literal mode, degraded to the bare letter `j`. The pearl
     debug log showed task prompts rendering as
     `affine-cipher (python).jjWorking directory: …jFiles present:j  -
INSTRUCTIONS.mdj…`. Switched `TmuxDriver::send` to
     `load-buffer` + `paste-buffer`, which inserts the payload as raw
     bytes — newlines, tabs, and Unicode all preserved verbatim. Added
     a regression test that pipes a 3-line message through `cat >
tmpfile` and asserts the file contains exactly 3 lines with no
     stray `j`s.

  2. **Driver LLM uses Claude-Code-style slash commands.** The
     default driver model (`smooth-summarize`) was emitting `/open`,
     `/read`, `/help` instead of plain English, which the TUI
     rejected as "Unknown command" and in two cases accidentally
     fired skills (`/add-show`, `/create-skill`). Hardened the system
     prompt + user prompt with explicit "no slash commands; you have
     no file/shell access — ask the assistant in plain English"
     directives, and added a slash-command guard in `run_human_loop`
     that drops `/`-prefixed turns, logs the violation to the
     pane-debug log, and re-asks the model with a reinforcement
     prompt. After 3 consecutive slash turns the loop bails with
     `LoopExit::Stuck` instead of burning the full turn cap.

  Tests added: `tmux_driver::send_preserves_newlines_no_j_leakage`,
  `human_driver::run_human_loop_marks_stuck_after_three_slash_commands`,
  `human_driver::run_human_loop_accepts_plain_english_message`, plus
  prompt-construction unit tests asserting the no-slash-commands
  language is present.

  Verified with a single-task `score-tui --pr --task-limit 1 --debug`
  smoke run: the new pane log shows the initial task prompt rendering
  with real newlines (zero `j` artifacts) and the driver's follow-ups
  all plain English (zero `/`-prefixed turns). The task itself failed
  (affine-cipher is a hard single-attempt task), but the harness is
  now healthy.

- d2483bc: bench: deterministic cost extraction via JSON sidecar. `score-tui` was reporting $0.00 across every task whenever the TUI pane-scrape regressed (status-bar format drift, ratatui repaint race against `tmux capture-pane`, ANSI bleed, in-flight `Completed` event). Now: `smooth-code` writes a `{cost_usd, iterations, ts_unix_ms}` JSON sidecar on `AgentEvent::Completed` when `SMOOTH_BENCH_COST_SIDECAR` is set, atomically (tmp→rename) and best-effort. `smooth-bench/tui_score` sets the env var to a per-task path under the run dir before spawning `th code`, then prefers the sidecar over the legacy pane-scrape. Falls back to scrape for older `th` binaries; falls back to $0.00 + a loud warning if both miss. Opt-in via env so plain `th code` sessions never drop a sidecar in the user's cwd. Pearl th-a08fa3.
- eac9b55: Pearl th-a5ca18: fix five score-tui bench harness bugs so a `--pr`
  sweep produces honest pass-rates with real cost numbers.

  The previous score-tui run reported 2/18 pass with $0.00 cost
  across all tasks, and tasks 15-18 errored at 0ms with `no server
running on /private/tmp/tmux-501/default`. Five independent bugs:

  **Bug 1 — tmux server dies mid-sweep.** The harness shared the
  default tmux socket across all tasks. When task N's `Drop` killed
  the last surviving session on that socket, tmux server-exited and
  every subsequent task got "no server running". Fix: per-task
  socket isolation via `tmux -L <socket>`. Each `TmuxDriver` gets a
  unique socket name; every `tmux …` invocation passes `-L`; `Drop`
  runs `kill-server` on its own socket only. New regression test
  `per_socket_isolation_survives_sibling_drop` verifies dropping one
  driver does not affect another's server.

  **Bug 2 — cost reported as $0.00 across all tasks.** The TUI's
  status line shows `spend: $X.XXX`, but the harness never scraped
  it. Fix: at task end, grab a visible-only capture (the status
  line is always in the visible region by definition), regex-extract
  the spend, and thread the value into the `TuiTaskOutcome::cost_usd`
  field. Falls back to 0.0 + warning when the pattern isn't found —
  never fabricates.

  **Bug 3 — Rust false-positive passes.** Both prior runs reported
  2/3 Rust passes on workspaces where `src/lib.rs` still held the
  dataset's `todo!()` macro. Root cause: the user's
  `~/.cargo/config.toml` sets `target-dir = ~/.cargo/shared-target`,
  so `cargo test` reused a previously-compiled test binary from an
  earlier successful run (verified by hand: running cargo test with
  the shared target dir → 10 passed; with `CARGO_TARGET_DIR` pointed
  at a per-task `<work_dir>/target` → 10 failed via todo!() panic).
  Two defences: (a) `score_work_dir` now sets `CARGO_TARGET_DIR` to
  a per-task isolated path so the shared cache can't leak across
  runs; (b) the harness hashes every editable file before the agent
  runs, re-hashes after, and refuses to mark a task solved=true when
  the agent made zero changes (`--allow-no-edit-passes` opts out for
  debugging).

  **Bug 4 — agents do real work but tasks still fail.** Investigation
  across five failed-task pane logs found this is tied to Bug 5
  (below): the agent IS writing code and spending money, but the
  LLM-as-human driver only sees the bottom slice of the pane via
  `tmux capture-pane -p`, so the driver keeps re-asking questions
  the agent has already answered. The agent's tool calls and edited
  content scroll off the visible region and the driver has no idea
  work happened. Fix follows directly from Bug 5.

  **Bug 5 — `capture-pane` blind to scrollback.** Confirmed by
  end-to-end read of
  `~/.smooth/bench-runs/e219203e/python-book-store.pane.log`: every
  `[idle]` capture shows the same bottom-of-pane slice (~50 rows of
  the input box + status line + last few wrapped lines of the most
  recent LLM response). The chat history, tool calls, and diffs are
  all in tmux's scrollback, invisible to the driver. Fix:
  `capture()` now passes `-S -` (start of scrollback) and `-J` (join
  wrapped lines), returning the full pane history. A
  `DEFAULT_CAPTURE_MAX_BYTES` (64 KiB) budget caps memory by
  truncating from the FRONT (dropping the oldest, keeping the
  freshest) with a marker prepended so the driver knows the very
  start was clipped. Added `capture_visible()` for the
  specific case (Bug 2) where we only want the bottom status line.

  Tests added (151 lib tests passing): per-socket isolation, full-
  scrollback capture, front-truncation budget + newline snapping,
  cost-extraction (real status line + repeated repaints + zero
  dollars + no-dollar-sign + malformed + dot-only forms), and
  hash-based editable-file detection across all five languages.

- f29eba3: Pearl th-cb3c2a: fix streaming buffer duplication (first-char doubling
  and whole-paragraph re-emit) in `smooth-operator`'s `chat_stream`.

  The OpenAI-compatible streaming path always treated `delta.content`
  chunks as incremental deltas and `push_str`'d them onto the running
  buffer. Some upstreams behind LiteLLM (and a few OpenRouter providers
  in certain modes) actually emit **cumulative** content per chunk —
  each chunk contains everything-so-far instead of the new tail. Treating
  those as deltas produced the quadratic blowup seen in
  `~/.smooth/coding-sessions/*.json`:

  - First-character doubling/tripling — `"I'll help you"` arriving as
    `"III'll help you"` because chunks `"I"`, `"I"`, `"I'll help you"`
    were all appended.
  - Word-level doubling — `"Let Let me me first first read read"` from
    `"Let"`, `"Let me"`, `"Let me first"`, `"Let me first read"` all
    appended verbatim.
  - Entire paragraphs repeated 3-4× within a single assistant message.

  The corruption then fed into the next turn's `prior_messages`, so the
  LLM saw its own garbled prior turn and tended to bail with "I don't
  have context" instead of calling tools — which is why the agent in
  the smoking-gun session emitted zero successful tool calls over 12
  turns.

  Fix: a per-stream `StreamContentNormalizer` between `parse_sse_line`
  and the consumer. For each chunk, if the chunk is exactly the
  accumulator (cumulative-restart), drop it; if it strictly extends the
  accumulator, emit only the new tail; otherwise treat as a normal
  delta. A separate per-tool-call-index normalizer applies the same fix
  to `ToolCallArgumentsDelta` chunks so cumulative argument streams
  can't produce double-encoded JSON. The normalizer is a no-op on
  well-behaved delta-emitting providers (every OpenAI/Anthropic stream
  we already ship through). Covered by seven new unit tests in
  `crates/smooth-operator/src/llm.rs`.

- 544e494: Pearl th-f46efa: fix `smooth-bench score-tui` tmux harness so it
  actually exercises `th code` instead of false-passing.

  PR #55's first `--pr` run finished 18 tasks in 12 minutes with
  2/18 pass and $0.00 cost — strong evidence the harness was broken,
  not Smooth. The score-tui-pr.log showed "no server running on
  /private/tmp/tmux-501/default" twice before every task and a
  median task wall-clock of 38s, far below the 900s per-task cap.
  The two Rust passes were false positives: aider-polyglot fixtures
  should not pass un-edited, so the harness was scoring workspaces
  the agent never touched.

  Root causes addressed:

  1. **Empty-pane false-idle in `wait_for_idle`**: the old heuristic
     ("byte-identical for 2s") declared a blank pane idle, so the
     LLM-as-human loop sent its first turn before `th code` had
     finished booting. `wait_for_idle` now takes a `min_bytes` floor
     (default 200 non-whitespace chars) — below the floor the pane is
     treated as still-rendering and we keep polling. New
     `wait_for_idle_with_floor` exposes the floor explicitly for
     tests.
  2. **Stale-state false-render in `wait_for_first_render`**: the gate
     accepted a single printable char as "rendered". Now requires
     the same 200-char floor before returning, so a brief artifact
     doesn't count.
  3. **`th code` boot timeout too short**: bumped default
     `TuiTaskConfig::boot_timeout` from 15s → 120s. `th code` brings
     up the Safehouse microVM + cast (wonk · goalie · narc · scribe
     · archivist · diver · groove) + operator-runner pool before the
     input prompt; empirically 30-60s on a warm machine. 15s was
     under, so the boot gate fired prematurely.
  4. **Tmux stderr noise**: `tmux has-session`, `tmux -V`, and the
     Drop's `kill-session` all printed "no server running" to stderr
     in the no-server-yet case (normal during probing). All probes
     now redirect stderr to `/dev/null`; real failures still surface
     the error text through `capture-pane`'s embedded stderr-in-error.
  5. **Stuck tasks were scored as passes**: aider-polyglot fixtures
     should not pass un-edited. New `stuck_means_failed` knob (on by
     default; bypass with `--allow-stuck-passes`) forces
     `solved=false` when the LLM-as-human driver bailed on turn 1
     without a `TASK_COMPLETE` sentinel — kills the silent
     corruption where un-edited Rust workspaces reported as solved.
  6. **$0 cost across the board is now a loud warning**: the harness
     prints a warning at the end of a sweep when every task reports
     $0.00, so future-us can't mistake an un-wired cost surface for
     "the run was meaningful but cheap".

  Diagnostics (`--debug`):

  - New `PaneDebugLog` type writes per-task `<lang>-<task>.pane.log`
    to the run dir with timestamped records at every `send`, every
    `wait_for_idle` boundary, AND the boot screen frames.
    `capture-pane` failures dump the last good capture so the op can
    see what the user saw before the session died.
  - New `--task-limit N` flag caps the sweep at N tasks (default 0 =
    no cap). Use `--task-limit 1 --debug` to exercise a single task
    end-to-end with full pane logging.

  Tested: existing tmux integration tests updated for the new
  boot-floor + 200-char idle threshold; new tests cover the floor
  rejecting empty panes, the debug log recording send/idle events,
  and the duplicate-session / drop-kills-session paths still pass
  with longer payloads.

- 50cac77: Pearl th-fcb579 — browser-based `th auth login` (smooth side). Lays the
  OAuth2 + PKCE plumbing for `th auth login` to open the user's default
  browser, capture the authorization code on a localhost listener, and
  exchange it for tokens — matching the `gh auth login` / `gcloud auth
login` UX. Behind the `SMOOTH_AUTH_BROWSER=1` env gate while the
  smooai-side `/cli-login` endpoint (pearl th-62e710) is in flight; new
  `--browser` / `--no-browser` flags let callers override the gate
  explicitly. Pairs with a single-store `active_org::set` writer that
  will swap to the cross-store writer from pearl th-3217db once that
  lands. New modules: `crates/smooth-cli/src/auth/pkce.rs` (RFC 7636
  code verifier + S256 challenge generator), `auth/browser_login.rs`
  (tiny_http listener, PKCE flow, token exchange), `auth/active_org.rs`
  (active-org-id persistence helper). Headless / SSH / CI paths
  unchanged — no TTY = no browser, ever. M2M (`--m2m`) unchanged.
- a061e67: GitHub release notes now lead with copy-pasteable install + upgrade commands instead of just the bare changelog. New `scripts/build-release-notes.sh` renders an Install section (Homebrew first, then `curl | sh`, then `cargo install`), an Upgrade section (one-liner per channel), the version's CHANGELOG.md extract, a Downloads table populated from the live release assets (with a fallback to the workflow's expected names when run before the release is created), and a footer linking the source / README / tap. Wired into `release.yml`'s `Create Release` job via `body_path`. v0.13.7 retroactively re-rendered with this format. Pearl th-release-notes.
- 26f7618: pearls: auto-heal a corrupt/unreadable Dolt store + fix the clone that left `main` empty

  Two linked fixes for the pearl-store corruption class that left the smooai
  monorepo's store unreadable (`open .../.dolt/repo_state.json: no such file or
directory`), independent of any pearl work.

  **Root cause (th-3f6657) — `smooth-dolt clone` left `main` at the empty init
  commit.** `cmdClone` did init + remote-add + `DOLT_PULL origin main`. The init
  root is always unrelated to the remote's history, so `DOLT_PULL` fetched all
  chunks into `remotes/origin/main` but refused to merge unrelated histories,
  silently leaving `main` on the empty init commit. Every fresh bootstrap clone
  came up "empty" (`table not found: pearls`) while physically holding the full
  pulled data. `cmdClone` now force-resets `main` onto the pulled remote head
  after the pull (no-op when the remote branch is absent).

  **Auto-heal (th-03cdb8) — wire recovery into the `th pearls` open path.** Any
  `th pearls` command now recovers on open instead of surfacing a raw smooth-dolt
  error:

  - `SmoothDolt::diagnose` now classifies a `.dolt/` dir that's missing
    `noms/manifest` or `repo_state.json` as recoverable `Corrupt` (the
    interrupted-GC/half-clone signature) rather than dead-end `NotInitialized`.
  - `recover_from_remote` resolves the origin from the enclosing git repo's
    `origin` when `repo_state.json` itself is the missing file, and normalizes
    the root/`pearls`-subdir layout so the re-clone lands correctly. It reuses
    `clone_from` (so it inherits the clone-reset fix above).
  - `PearlStore::open` runs the recovery on first-touch failure (snapshot the
    broken store aside, re-clone from origin, re-open), loudly to stderr.
    CLI-mode only — it never re-clones out from under a live `smooth-dolt serve`
    (Big Smooth); those cases point to `th pearls doctor --force`.

  Canonical pearl data lives on the remote's `refs/dolt/data` under the beads
  model, so the re-clone is non-destructive. Covered by new unit tests for the
  diagnose classification and the git-origin fallback.

- 10c82aa: th pearls: quiet auto-commit under beads model + fix smooth-dolt status

  Two follow-ups to pearl `th-975dfe` (beads-model migration):

  **Pearl `th-016296`**: `auto_commit_pearl_state` now detects that
  `.smooth/dolt/` is git-ignored (via `git check-ignore -q`) and
  silent-noops instead of erroring on `git add .smooth/dolt/` with
  "use -f to force-add ignored files". Sync stays via `th pearls push`
  to `refs/dolt/data`; no git commits are needed for the on-disk store
  under the beads model. Repos that haven't migrated yet (still track
  `.smooth/dolt/`) keep the legacy auto-commit path.

  **Pearl `th-f6c50c`**: `smooth-dolt status` previously called
  `CALL DOLT_STATUS()` which errored with "stored procedure does not
  exist". DOLT*STATUS is a \_system table* in Dolt, not a procedure or
  table function. Fix: `SELECT table_name, staged, status FROM
dolt_status` in both the CLI handler (`cmdStatus`) and the
  socket-mode handler (`doDoltCmd`). Clean working set → empty output
  (preserves the pre-commit hook's `.trim().is_empty()` contract);
  changed tables → one line per row.

  4 new tests covering `is_dolt_gitignored` (true / false / non-git)
  and `auto_commit_silent_noop_when_dolt_gitignored`.

- 8c801ff: Pearl th-893801 Phase 1 iter-1 (spike). Wires tonic + prost +
  tonic-build into the workspace and proves the gRPC machinery
  works end-to-end with the smallest possible slice: the Narc
  service compiled from `proto/narc.proto`, served over a UDS, and
  exercised by a tokio test that round-trips Judge calls.

  The smooth-narc crate now exposes `pb` (generated proto types),
  `convert` (TryFrom/From between proto types and the existing
  in-crate `judge::*` types), and `grpc` (a tonic server adapter
  that wraps a `Judge` trait — implemented by the test stub here;
  production impl in smooth-bigsmooth's BoardroomNarc lands in
  iter-2). 13 new tests across conversions + UDS round-trips.

  Iter-2 picks up the rest of Phase 1: wonk + scribe + bigsmooth
  proto servers, then operator-runner client switch, then the
  SMOOTH_SINGLE_PROCESS feature flag.

- 5d9b675: Pearl th-893801 Phase 1 iter-2. Applies iter-1's tonic-over-UDS
  pattern to the three remaining cast crates: Wonk, Scribe, and
  Big Smooth. Each gets a `pb` module (tonic-generated types), a
  `grpc` module (server adapter wrapping a small per-service
  trait), and `serve_uds` for spawning the server on a Unix socket.

  - **smooth-wonk** — Wonk service over UDS. `Checker` trait
    abstracts CheckNetwork/Tool/Cli/File + ReloadPolicy/Summary.
    Verdicts carry `was_escalated` + `resolved_scope` so callers
    can distinguish policy-decided from human-resolved approvals.
    Wonk's proto imports narc.proto for `Scope`; tonic-build
    routes through smooth-narc's existing `pb` module via
    `extern_path`.
  - **smooth-scribe** — client-streaming Log + server-streaming
    Query. `Logger` trait abstracts append/query/stats. mpsc
    channel back-pressures the store walker on slow consumers.
  - **smooth-bigsmooth** — Dispatch + Cancel + AccessStore
    CRUD + AccessEvents/OperatorEvents server-streams. The
    `Orchestrator` trait is wide (10 methods) but each method
    maps 1:1 to a proto RPC. Production wiring (into the
    existing AppState + AccessStore) lands in iter-3.

  Proto-include change: both wonk.proto and bigsmooth.proto now
  import `"narc.proto"` (relative within the workspace proto/
  root) instead of the full `"smooth/narc/v1/narc.proto"` package
  path. Cleaner with our flat proto/ layout.

  17 new tests across the three crates:

  - wonk (5): network allowed/denied, tool round-trip, file
    Unspecified→InvalidArgument, resolved_scope flow-through.
  - scribe (4): Log client-streaming, Query server-streaming,
    GetStats, back-pressure drop.
  - bigsmooth (4): Dispatch, AccessStore CRUD round-trip,
    AccessEvents stream, OperatorEvents stream.

  Iter-3 picks up: the production trait impls (BoardroomNarc as
  Judge, AppState as Wonk Checker + Scribe Logger + BigSmooth
  Orchestrator), the operator-runner client switch, and the
  SMOOTH_SINGLE_PROCESS feature flag that selects the new path.

- f413174: Pearl th-893801 Phase 1 iter-3a. Production wiring: BoardroomNarc
  implements smooth_narc::grpc::Judge so the existing decision flow
  serves the new gRPC Narc surface unchanged. The trait's signature
  already matched BoardroomNarc::judge — this is mostly the impl
  declaration plus a `narc_grpc::serve_uds` wrapper for the BS
  startup glue (iter-3e).

  7 new tests in smooth-bigsmooth — drive the real BoardroomNarc
  over UDS gRPC end-to-end: rule-engine approve for npmjs.org,
  rule-engine deny for pastebin.com, EscalateToHuman for unknown
  domains without an LLM, persistent-grant short-circuit, cache-len
  round trip through GetCacheStats, sanity check that the trait
  routes to the inherent method.

- febb88f: Pearl th-893801 Phase 1 iter-3b. Production wiring: Wonk's
  existing `AppState` implements `smooth_wonk::grpc::Checker`, so
  the same policy + Narc-escalation logic that drives the
  `/check/*` HTTP handlers now drives the gRPC `CheckNetwork`,
  `CheckTool`, `CheckCli`, `CheckFile`, `ReloadPolicy`, and
  `PolicySummary` RPCs. Iter-3e will spawn this in-process over a
  UDS when `SMOOTH_SINGLE_PROCESS=1` is set.

  Decision logic intentionally mirrors the HTTP handlers in
  `server.rs` for this iter — the dedup happens in Phase 4
  cleanup once the HTTP surface is retired. The Checker still
  escalates to Narc via the existing HTTP `NarcClient` (option
  (a) in the plan); iter-3f swaps that for the gRPC client.

  10 new tests in `smooth-wonk` exercise the trait end-to-end:
  static-allowlist approve, auto-approve-domain approve,
  unknown-domain deny, tool allow/deny/unknown, file inside-mount
  allow + outside-mount + traversal deny, dangerous-CLI flag, the
  PolicySummary RPC, and a sanity check that the trait routes
  into AppState's policy holder.

- d365bdf: Pearl th-893801 Phase 1 iter-3c. Production wiring:
  `GrpcLogStoreAdapter` implements `smooth_scribe::grpc::Logger`
  on top of any `LogStore`, including the existing
  `MemoryLogStore`. The proto Scribe surface (Log /
  Query / GetStats) now drives the same in-memory ring the legacy
  `/log` HTTP endpoint feeds.

  The domain `LogEntry` predates the proto contract by a wide
  margin, so this module owns the proto<->domain conversion.
  Lossy in two well-defined ways:

  - `pb::Level::Trace` and `pb::Level::Unspecified` fold to
    domain `Debug` / `Info` (domain has no Trace).
  - Domain `id` (uuid) has no proto equivalent — generated on
    append, dropped on emit. Queries match on the rest.

  The proto QueryRequest is richer than the in-store `Query`
  (since/until/operator_id/bead_id/trace_id/message_contains
  on top of source/min_level/limit). The cheap subset is pushed
  to the store; the rest is applied in-process during the walk.

  9 new tests in `smooth-scribe` cover level/entry round-trips,
  client-streaming append, server-streaming query with source +
  min-level + case-insensitive message filters, GetStats's
  total_entries counter, and the `adapter_for_memory_store()`
  convenience.

  Tech-debt: forwarder.rs + hook.rs + log_entry.rs + server.rs +
  store.rs have narrow `#![allow(clippy::expect_used)]` annotations
  for pre-existing `.expect()` calls so iter-3c's quality gate
  runs cleanly. The forwarder + HTTP server retire in Phase 4
  once the gRPC Scribe is the only ingest path; cleanup happens
  then rather than in this iter.

- 365eb0c: Pearl th-893801 Phase 1 iter-3d. Production wiring of the
  `BigSmooth` gRPC `Orchestrator` trait via the new
  `OrchestratorAdapter` over the existing `AccessStore`.

  Fully wired RPCs (same semantics as the `/api/access/*` HTTP
  routes):

  - `FilePendingAccess` — files into the AccessStore, returns
    the freshly-stamped id + timestamp. Surfaces invalid
    `JudgeKind` as an empty id (the trait signature is
    infallible by design — clients detect via the empty id).
  - `ResolveAccess` — drives `AccessStore::resolve`, mapping
    proto Verdict/Scope into the domain enums and surfacing
    `NotFound` / `InvalidArgument` via `tonic::Status`.
  - `ListPendingAccess` — snapshot of currently-pending
    requests as proto `PendingAccess` messages.
  - `SubscribeAccessEvents` — server-streams every
    Pending/Resolved/Expired event from the AccessStore's
    broadcast channel; recovers cleanly from `Lagged` and ends
    on client cancel.

  Stubbed RPCs (land in Phase 2 / pearl th-ea2aa5 once `th up`
  exists):

  - `Dispatch`, `Cancel` — return `Unimplemented` with a clear
    pointer to the pearl.
  - `ListOperators` — returns an empty list (bench harness
    probe needs this to not error).
  - `SubscribeOperatorEvents` — returns immediately, ending the
    stream gracefully.

  11 new tests in `smooth-bigsmooth` cover end-to-end round trips
  over UDS for file/resolve/list/subscribe, plus the kind +
  scope round-trip helpers, the unspecified-kind error path, the
  not-found resolve path, and the dispatch/list-operators stubs.

- edb24bb: Pearl th-893801 Phase 1 iter-3e. New
  `smooth_bigsmooth::single_process` module brings the four cast
  gRPC servers (Narc/Wonk/Scribe/BigSmooth) up on UDS sockets in
  one shot when `SMOOTH_SINGLE_PROCESS=1` is set.

  Socket layout under `socket_dir()`:

  - `$SMOOTH_SINGLE_PROCESS_SOCKET_DIR/{narc,wonk,scribe,bigsmooth}.sock` — explicit override (tests).
  - `$XDG_RUNTIME_DIR/smooth/` — XDG-compliant default.
  - `/tmp/smooth-<pid>/` — last-resort fallback.

  `bootstrap_grpc_cast` returns a `GrpcCastHandles` owning the
  four `JoinHandle`s + socket paths + the fresh `MemoryLogStore`
  the Scribe gRPC writes into. `shutdown()` aborts the tasks
  and removes the socket files.

  `bootstrap_from_app_state` is the BS-specific helper that
  pulls `BoardroomNarc` + `AccessStore` straight from the
  existing `AppState` and seeds a fresh Wonk `AppState` with a
  permissive default policy (mirrors the legacy boardroom
  spawn). The boardroom binary now invokes this after
  `AppState::new` so the gRPC cast comes up co-resident with
  the legacy HTTP cast — iter-3f will rewire the operator-runner
  to dial the UDS sockets instead.

  4 new tests: env-var contract, all-four-sockets-exist after
  bootstrap, end-to-end gRPC round-trip per socket (Narc
  GetCacheStats / Wonk PolicySummary / Scribe GetStats /
  BigSmooth ListPendingAccess including a freshly-filed
  request), and shutdown removes the socket files.

- cadedba: Pearl th-893801 Phase 1 iter-3f. New
  `smooth_bigsmooth::tonic_clients` module providing UDS-dialing
  client adapters for the in-VM cast. Method signatures mirror
  the legacy HTTP clients so runner call sites can swap them in
  without a rewrite:

  - `NarcGrpcUds::judge(&JudgeRequest) -> JudgeDecision` — drop-in
    for `smooth_wonk::NarcClient`. Folds any transport / proto
    error into `EscalateToHuman` so Wonk fails closed.
  - `ScribeGrpcUds::append(pb::LogEntry) -> bool` — replaces the
    HTTP Archivist forwarder with a client-streaming gRPC Log
    RPC. Entries are queued through a bounded mpsc and the
    background task owns the stream.
  - `BigSmoothGrpcUds` — wraps the generated `BigSmoothClient` so
    callers can dial a UDS path instead of a hostname; exposed
    via `.client()` since the AccessStore RPC surface is large
    enough that callers want the full generated client.
  - `GrpcCastClients::connect_all(socket_dir)` — convenience
    bundle resolving the three sockets against the standard
    `single_process::bootstrap_grpc_cast` layout.

  Wiring into the operator-runner is deliberately deferred — the
  adapters land first so iter-3g's smoke test can exercise them
  end-to-end against `bootstrap_grpc_cast`. Phase 2 will replace
  the runner's own cast-spawn path with these adapters when the
  runner is co-resident with BS in the single VM.

  5 new tests: Narc round-trip approves a safe domain over UDS,
  Narc folds a dead socket to EscalateToHuman with the expected
  reason, Scribe streams 3 entries that land in the gRPC-backed
  MemoryLogStore, BigSmooth lists a freshly-filed pending
  request, and `connect_all` resolves the standard socket
  layout.

  Also: moved `hyper-util` from dev-deps to deps on the
  bigsmooth crate so the UDS connector code compiles outside
  the test cfg.

- 1f80fde: Pearl th-893801 Phase 1 iter-3g. End-to-end smoke test for the
  single-VM gRPC cast — confirms iter-3a..3f wire together as a
  system. Lives in `crates/smooth-bigsmooth/tests/` so it doesn't
  share a socket-dir namespace with the parallel unit tests.

  Coverage:

  - `single_process_cast_round_trips_a_narc_then_resolve_flow` —
    bootstrap → connect_all → Narc.judge auto-approves a known
    safe domain → BigSmooth.file_pending_access seeds the store
    → list shows the pending entry → Scribe streams five entries
    that land in the gRPC-backed MemoryLogStore → AccessStore
    resolution clears the pending list. All five RPCs cross UDS.
  - `bootstrap_shutdown_rebootstrap_cycle_works` — exercises the
    shutdown path's socket-unlink contract by re-bootstrapping
    against the same directory.

  Closes the gRPC-collapse arc for Phase 1: each cast member has
  its wire surface (iter-2), each is production-wired (iter-3a..d),
  BS spawns them on UDS under the flag (iter-3e), client adapters
  exist for the runner (iter-3f), and the smoke confirms it
  holds together (iter-3g). Phase 2 (pearl th-ea2aa5) flips the
  sandbox topology to put the runner in the same VM as BS so it
  actually dials these sockets.

- 2b7978b: Pearl th-893801 Phase 2 iter-4a. New `smooth-host-stub` crate
  — the credential broker that runs on the macOS host and
  bridges the single sandbox VM to host-resident CLIs.

  The sandbox sees a UDS bind-mounted at
  `/run/smooth/host.sock` and dials this server when an in-VM
  tool needs a credential for a known server (GitHub, AWS, GCR,
  ECR, …). The stub matches the `server_url` against registered
  backends' globs, validates readiness, and shells the matched
  backend out for a fresh credential.

  Surface shipped in this iter:

  - `Backend` trait + `BackendInfo` / `CredentialRequest` /
    `IssuedCredential` / `BackendError` domain types.
  - `BackendRegistry` — registration order matters (first
    matching glob wins); routes `issue` by `server_url`.
  - `glob_matches` — handles exact hostnames, `*.foo.com`
    subdomain wildcards, and falls back to full glob semantics.
  - `HostStubServer` — tonic adapter mapping `IssueCredential`
    and `GetCredentialBackends` onto the registry. Backend
    errors map to the right gRPC `Status` codes
    (`NotFound` / `FailedPrecondition` / `InvalidArgument` /
    `Internal`).
  - `serve_uds` — bind-and-spawn helper.
  - `smooth-host-stub` binary that reads
    `SMOOTH_HOST_STUB_SOCKET` (default `/run/smooth/host.sock`)
    and serves an empty registry. Concrete backends (gh,
    aws-sts, gcloud, az-acr) land in follow-up iters once the
    shellout audits are reviewed.

  15 new tests: glob matching across exact/subdomain/path
  strips; registry routing including unknown-server,
  not-ready, empty-URL, and overlap-resolution paths;
  end-to-end gRPC round trips for issue/list/empty/unknown
  over UDS; trait + enum coverage.

- b517990: Pearl th-893801 Phase 2 iter-4b. First concrete host-stub
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

- cbd27a3: Pearl th-893801 Phase 2 iter-4c. `AwsStsBackend` wraps
  `aws sts get-session-token` and `aws sts assume-role`.

  Design decisions resolved in this iter:

  - **session_token packaging**: added `session_token` (proto field 5)
    to `IssueCredentialResponse`. Additive — older clients ignore
    it. The alternative of JSON-packing into `secret` would have
    broken the Docker credential-helper-shaped contract the proto
    is built around.
  - **scope_hint mapping**:
    - `Read` / `Unspecified` → `sts get-session-token`
    - `Write` with `SMOOTH_AWS_WRITE_ROLE_ARN` set →
      `sts assume-role --role-arn … --role-session-name smooth-<op>`
    - `Write` without the env var → falls back to
      `get-session-token` and logs a warning.
  - **env var racing**: the role-ARN env is read once at
    construction (`AwsStsBackend::with_runner` / `::new`); tests
    override via `with_write_role_arn(...)` rather than mutating
    the process env, so parallel test runs can't race.

  Domain `IssuedCredential` gains a `session_token: Option<String>`
  field; existing backends (`GitHubBackend`, test fakes) set it to
  `None`. The HostStubServer adapter threads it onto the wire,
  defaulting to an empty string when `None`.

  10 new tests: default-glob check; `Read`/`Unspecified` →
  get-session-token; `Write` with role-arn → assume-role; `Write`
  without role-arn → get-session-token fallback; STS CLI failure
  → Mint; malformed JSON → Mint; missing session_token → Mint;
  RFC3339 expiration parses; garbage expiration → None.

- a376d87: Pearl th-893801 Phase 2 iter-4d. `GcloudBackend` wraps
  `gcloud auth print-access-token` to mint OAuth access tokens
  for in-sandbox GCP calls.

  Default globs:

  - `gcr.io`, `*.gcr.io` — Container Registry
  - `*.pkg.dev` — Artifact Registry (regional hosts like
    `us-central1-docker.pkg.dev`)
  - `*.googleapis.com` — every Google Cloud API

  `ScopeHint` is ignored — the token's IAM permissions decide
  read vs write. Output is the raw token; we use the literal
  `oauth2accesstoken` as the username (matches Google's
  container-registry credential helper convention).

  Error mapping:

  - stderr containing "credentials" + "not" →
    `NotReady` ("gcloud CLI not logged in: …").
  - empty stdout → `Mint`.
  - other CLI failures → `Mint` with the trimmed stderr.

  6 new tests: default globs, override, happy-path token,
  logged-out → NotReady, empty token → Mint, generic failure
  → Mint.

- cfb7bd5: Pearl th-893801 Phase 2 iter-4e. `smooth_host_stub::docker_socket`
  auto-detects the host's Docker-compatible socket so `th up`
  can bind-mount it into the sandbox transparently regardless
  of which container runtime the user installed.

  Probe order (first match wins):

  1. `DOCKER_HOST` env (`unix://` scheme only — `tcp://` is
     rejected with a clear error since it can't be bind-mounted).
  2. Colima: `$HOME/.colima/default/docker.sock`.
  3. OrbStack: `$HOME/.orbstack/run/docker.sock`.
  4. Rancher Desktop: `$HOME/.rd/docker.sock`.
  5. Podman (rootless): `$XDG_RUNTIME_DIR/podman/podman.sock`.
  6. Docker Desktop default: `/var/run/docker.sock`.

  The probe is filesystem-only (no `docker ps` shellout) so it
  runs synchronously at startup. Returns a `DetectedSocket`
  with the resolved path and a `DockerRuntime` label `th up`
  surfaces ("using Colima at …").

  `FsProbe` trait abstracts filesystem + env access; tests
  inject a `StubProbe` with canned `exists` / `env_var` /
  `home_dir` answers, no /tmp scribbling needed.

  11 new tests cover every probe branch: DOCKER_HOST happy
  path / tcp rejection / unix-missing error; each runtime path
  in isolation; ordering preference between Colima and
  OrbStack; Podman via XDG_RUNTIME_DIR; Docker Desktop last
  resort; total miss → `NotFound`; label rendering.

- 317a2ad: Pearl th-893801 Phase 2 iter-4f. New
  `docker/Dockerfile.smooth-vm` + `scripts/build-smooth-vm-image.sh`
  build the long-lived sandbox image `th up` boots
  (iter-4g lands the lifecycle command).

  Design choices:

  - **Base**: `debian:bookworm-slim`. Cloud CLIs (gcloud, az)
    install cleanly without the glibc/musl friction that
    blocks them on the alpine boardroom base. ~80MB before
    layering CLIs.
  - **Cloud CLIs bundled via vendor scripts**, each pinned via
    build ARG so rebuilds are reproducible: `gh@2.62.0`,
    `awscli v2` (latest), `gcloud@494.0.0`, `az@2.66.0`,
    `kubectl@1.31.4`, `docker CLI@27.4.1` (CLI only — the
    host's daemon socket is bind-mounted at `/var/run/docker.sock`).
  - **mise** (pinned to `2024.12.13`) for language toolchains.
    Seeded `~/.config/mise/config.toml` ships node 22 / python
    3.13 / go 1.23; users `mise install <other>` after the VM
    is up and state persists in `/root` (volume).
  - **Smooth binaries** copied from
    `target/aarch64-unknown-linux-musl/release/`: `boardroom`,
    `smooth-operator-runner`, `smooth-dolt`.
  - **Long-lived state**: `/workspace` is the bind-mounted
    user repo; `/root` is a named volume carrying mise state,
    pearl DB, SSH config, gh/aws/gcloud credentials. `th down`
    stops the container without touching the volume; `th prune`
    (iter-4g) removes the volume.
  - **Env defaults**: `SMOOTH_SINGLE_PROCESS=1`,
    `SMOOTH_BOARDROOM_MODE=1`,
    `SMOOTH_HOST_STUB_SOCKET=/run/smooth/host.sock`. The
    in-process gRPC cast comes up on UDS sockets under
    `$XDG_RUNTIME_DIR/smooth/` by default.

  `scripts/build-smooth-vm-image.sh` mirrors the existing
  `build-boardroom-image.sh` ergonomics (`--push`, explicit
  version arg, `SMOOTH_IMAGE_REPO`/`SMOOTH_IMAGE_TOOL` env
  overrides) and cross-compiles the three required binaries
  before invoking `docker build`. Default repo:
  `ghcr.io/smooai/smooth-vm`.

  iter-4g wires the `th up` / `th down` / `th prune` lifecycle
  on top of this image.

- 2911c39: Pearl th-893801 Phase 3 iter-5a. New
  `smooth_pearls::memory` module providing CRUD over the
  `memories` table that's existed in the pearl Dolt schema
  since day one but had no API.

  `MemoryStore::new(SmoothDolt)` constructor; methods:

  - `append(content, source)` — insert with a fresh
    `mem-XXXXXX` id; rejects empty content.
  - `list_recent(limit)` — newest-first, capped.
  - `list_by_source(source, limit)` — filter to a specific
    origin tag (a pearl id, an operator id, `"manual"`, …).
  - `count()` / `clear_by_source(source)` / `clear_older_than(cutoff)`.

  The `source` field is the join key that lets us recall
  "everything the agent learned working on `th-abc123`" or
  "everything written by operator-7". Append-only API on
  purpose; pruning is bulk-by-source or bulk-by-age.

  SQL quoting via single-quote doubling (Dolt's CLI doesn't
  expose prepared statements). 8 new tests cover round-trips,
  filter-by-source, limit honoring, both clear paths, the
  empty-content guard, and a single-quote-in-content insert.

  Dolt's `DATETIME` column has 1-second resolution so two
  inserts within the same second tie on ordering; documented
  in the API + tests cover both the ≥1s-apart case (strict
  ordering) and the same-second case (every row retrievable
  but order unspecified). Production callers write seconds /
  minutes apart so this isn't a real constraint — long-term
  we'd switch to TIMESTAMP(3) or a sortable insert sequence.

  iter-5b will wire this into the dispatch path so the agent
  sees recent notes on task start and can write new ones on
  completion.

- 94ec744: Pearl th-893801 Phase 3 iter-5b. Agent-callable tools backed
  by the iter-5a `MemoryStore`. Lets the agent decide when to
  write learned-context notes and read them back without
  touching the dispatch path.

  Three tools registered via
  `smooth_pearls::register_memory_tools(registry, store)`:

  - `remember(content, source?)` — append a note. `source`
    defaults to `"manual"`; agents typically tag with their
    current pearl id.
  - `recall_recent(limit?)` — newest-first list; default 20,
    clamped to [1, 100]. Returns "no remembered notes yet"
    when empty.
  - `recall_by_source(source, limit?)` — filter to a specific
    origin (a pearl id, an operator id). Useful for "pick up
    where I left off on `th-abc123`".

  Read-only flags are wired so callers that gate writes can
  distinguish — `recall_*` are read-only; `remember` is not.

  Tool descriptions emphasize concrete short notes — the agent
  should remember facts, gotchas, commands, paths, not full
  sentences of narrative. The system prompt is what teaches
  the agent when to invoke these (top of task = `recall_recent`,
  end of task = `remember`).

  7 new tests cover the happy round-trip, empty-store friendly
  message, source-filter behavior, default-source fallback,
  missing-content error, limit clamping at both ends, and the
  read-only-flag advertisements.

  iter-5b is the last Phase 3 deliverable. The agent-side
  system-prompt nudges to actually call these can land
  incrementally without another iter — that's a prompt edit,
  not architecture.

- 579cef9: Pearl th-893801 Phase 4 iter-6a. First cleanup slice — drops
  the "boardroom" framing from the user-facing surfaces while
  keeping the legacy names alive for back-compat during the
  transition.

  Changes:

  - New `smooth_bigsmooth::Narc` re-export — alias for
    `BoardroomNarc`. New code in single-VM-mode paths should
    reference `Narc`; the struct itself stays at its current
    module path so existing imports keep working.
  - New env var `SMOOTH_VM_MODE` — preferred over
    `SMOOTH_BOARDROOM_MODE`. `server::start` honors either
    during the transition (new wins when both set).
  - `boardroom` binary now sets both `SMOOTH_VM_MODE=1` and
    `SMOOTH_BOARDROOM_MODE=1` on startup so the binary works
    with both old and new flag readers.
  - `Dockerfile.smooth-vm` exports both env vars so a container
    built before Phase 4 lands fully still satisfies any
    legacy check.
  - Log message in `server::start` rephrased: "Big Smooth
    running with in-process cast" — drops the
    "Boardroom mode" framing.

  No type renames yet — `BoardroomNarc`, `BoardroomHandles`,
  `crate::boardroom::*` all stay where they are. Renaming the
  types is iter-6b once we're confident the aliases haven't
  broken anything.

  271 bigsmooth tests still pass.

- a29677a: Pearl th-893801 Phase 4 iter-6b. Host-tool gate. In single-VM
  mode the bundled CLIs (gh, aws, gcloud, az, kubectl, docker)
  are right there in the VM and the host-stub mints credentials
  over UDS — the legacy `host_tool` indirection through Big
  Smooth's `/api/host/exec` endpoint is unnecessary.

  operator-runner now skips `host_tool` registration when
  `SMOOTH_SINGLE_PROCESS=1`. The agent falls through to
  `BashTool` for the same CLIs, still mediated by Wonk's
  `check_cli` + Narc audit just like every other shell call.
  Logs the skip ("CLIs run directly in-VM") so the path is
  visible from the runner output.

  Legacy multi-VM dispatch (no `SMOOTH_SINGLE_PROCESS`) is
  unchanged — the existing host_tool path stays live.

  No new tests — single behavioral gate, exercised end-to-end
  by the iter-3g smoke test once the runner is co-resident
  with BS in Phase 2. Will retire `host_tool` entirely in a
  later iter once the legacy path is gone.

- e8d662c: Pearl th-893801 Phase 4 iter-6c. Finishes the runner-side
  gRPC Narc wiring iter-3f left as a TODO. Wonk's escalation
  slot now accepts either HTTP or UDS transport, and the
  operator-runner picks the right one at startup.

  Shape of the change:

  - New `smooth_wonk::NarcEscalator` trait —
    `async fn judge(&self, request: &JudgeRequest) -> JudgeDecision`.
    Implementors must fail closed on any transport error so the
    contract matches the legacy HTTP client.
  - The legacy `NarcClient` (HTTP) impls the trait.
  - New `smooth_wonk::NarcGrpcUds` — UDS-dialing gRPC client
    implementing the same trait. Moved from
    `smooth_bigsmooth::tonic_clients::NarcGrpcUds` so wonk is
    the canonical home (it's the crate that needs to USE a
    Narc client). `smooth_bigsmooth::tonic_clients::NarcGrpcUds`
    is now a re-export for back-compat with iter-3f imports.
  - `AppState::with_narc` now takes any `NarcEscalator` impl —
    HTTP `NarcClient`, the new UDS client, or a test stub. The
    internal field is `Option<Arc<dyn NarcEscalator>>`. A new
    `with_narc_arc` accepts a pre-Arc'd value for callers
    hot-swapping clients.
  - operator-runner's `spawn_cast` now branches on
    `SMOOTH_SINGLE_PROCESS=1`: when set, dial Narc via UDS at
    `$XDG_RUNTIME_DIR/smooth/narc.sock` (override via
    `SMOOTH_SINGLE_PROCESS_SOCKET_DIR`). Else keeps the legacy
    `SMOOTH_NARC_URL` HTTP path. UDS connect failure logs and
    proceeds with no arbiter (Wonk hard-denies non-allowlisted
    requests, same fail-closed shape).
  - `Cargo.toml`: `tower` + `hyper-util` move from wonk
    dev-deps to deps so the UDS client compiles outside test
    cfg.

  3 new tests in `smooth-wonk::narc_grpc_uds`: round-trip
  approve over UDS via a stub Judge server; dead-socket-after-
  connect folds to EscalateToHuman; missing-socket connect
  errors with a clear message. The two equivalent tests in
  `smooth-bigsmooth::tonic_clients` are dropped (they
  exercised the same paths from a duplicate impl).

  75 wonk tests pass; 269 bigsmooth lib tests pass; iter-3g
  smoke test passes after a single `use smooth_wonk::NarcEscalator`
  import (the trait must be in scope to call `.judge` through
  the trait object).

- 2cb433e: Pearl th-893801 Phase 4 iter-6d. Naming aliases — extends
  iter-6a's `Narc` alias to the rest of the boardroom surface.
  Existing call sites keep working unchanged; new code prefers
  the cleaner names.

  New aliases in `smooth_bigsmooth`:

  - `vm_cast` — module alias for `boardroom`.
    `crate::vm_cast::*` and `crate::boardroom::*` resolve to
    the same items.
  - `VmCastHandles` — type alias for `boardroom::BoardroomHandles`.
  - `spawn_vm_cast` — fn alias for `boardroom::spawn_boardroom_cast`.

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

- 5234b72: build: make `smooth-cast` track the workspace version (`version.workspace = true`)

  `crates/smooth-cast/Cargo.toml` hardcoded `version = "0.13.7"` while every
  other workspace crate uses `version.workspace = true`. When the changeset
  Version PR bumped the workspace to `0.14.0`, all siblings followed but
  `smooth-cast` stayed `0.13.7`, so `cargo build --examples --workspace` failed
  with "failed to select a version for `smooai-smooth-cast = ^0.14.0` … candidate
  0.13.7 … required by smooai-smooth-bench v0.14.0", blocking the version PR's
  Rust checks (and thus the publish). Only exposed once `changeset version`
  finally ran end-to-end. Pearl th-d050a3.

- b7fd3ee: smooth-dolt: forward push/pull args into CALL DOLT\_\*() (pearl th-9eb6a0)

  `smooth-dolt push <dir>` previously called `CALL DOLT_PUSH()` with no
  args, silently dropping any trailing `-u origin <branch>` / `-f` flags
  that the Rust CLI appends for first-push auto-retry. First push to a
  fresh remote (smooblue today, any new project tomorrow) returned
  `fatal: The current branch main has no upstream branch.` and stayed
  errored even though the Rust matcher detected the case and called
  `push --set-upstream origin main`.

  Fix: parse `os.Args[3:]` and bind each as a positional SQL arg to
  `CALL DOLT_PUSH(?, ?, ?)` / `CALL DOLT_PULL(?, ?, ?)`. Zero-arg
  callers stay on the no-parens form so behavior is unchanged for them.

- 6d90c6a: th config set: consistency + hardening (pearl th-7ea946)

  Brings `set` in line with `get`'s flag surface, plus four hardenings:

  - **`--json`**: emits the API response as JSON, mirrors `get --json`.
    JSON output is never masked — caller asked for the wire shape.
  - **`--reveal`**: opt-in plaintext echo on `set` and `list`. Mask is
    the default (pearls th-4ebbf7 + th-9cc412); `--reveal` mirrors
    `scripts/secret-helpers/sst-secret-list --reveal` (CLAUDE.md §13).
  - **`--tier` as `ValueEnum`**: `public` / `secret` / `feature_flag`
    validated at parse-time. Typos like `--tier=secrets` now error
    with a list of valid options instead of round-tripping to the API
    and failing with a less-actionable 4xx.
  - **Empty-value reject**: `th config set FOO ""` (or whitespace-only)
    fails at parse-time with `value cannot be empty or whitespace-only`,
    not silently after the API call.

  Drops the `DEFAULT_TIER` `&str` constant in favor of `Tier::default()`
  so the default tier and its wire format are colocated. Tier wire
  format is locked by a test so a snake_case → camelCase regression
  can't sneak past.

- a7aa46f: th config: mask all echoed values (last-4 disclosure) regardless of tier

  `th config set` previously echoed public-tier values raw and secret-tier
  values masked to the last 4 characters. `th config list` echoed
  everything raw with no tier-awareness at all — same class of footgun
  as raw `pnpm sst secret list` (CLAUDE.md §13, SMOODEV-908).

  Tier no longer affects the echo. Both `set` and `list` mask every
  value to its last 4 characters. Public-tier keys can still be
  sensitive (CDN tokens, allowlist entries, anything an attacker could
  correlate) and the UX cost of `***wert` over `password-qwert` is
  trivial vs the cost of training users that console echo is a safe
  confirmation surface.

  `th config get` is unchanged — it's an explicit retrieval, not a
  side-effect echo, and reveal-on-demand is the right contract there.
  A future `--reveal` flag for explicit unmasking on `set` / `list`
  remains open as a follow-up if the UX hurts.

  Pearls th-4ebbf7 + th-9cc412.

## 0.13.7

### Patch Changes

- 8c66879: wonk/narc: close the loop on auto-mode Phase A. Safehouse Narc now
  holds tool calls open when its verdict is `Ask` — files into the
  shared `AccessStore`, awaits a human resolution with a 60s timeout,
  returns Approve / Deny / EscalateToHuman accordingly. New HTTP routes
  make the queue addressable from the TUI / CLI:

  - `GET /api/access/pending` — list of pending requests
  - `POST /api/access/approve` — resolve at a scope (once / session /
    project / user) with an optional glob override
  - `POST /api/access/deny` — same shape as approve
  - `GET /api/access/stream` — SSE feed of pending / resolved / expired
    events for inline UIs

  Low-confidence LLM approvals now coerce to `Ask` instead of silent
  `EscalateToHuman`, so the human gets agency over uncertain calls
  instead of just denials. `th access approve/deny <id> [--scope=...]
[--glob=...]` adopts the new id-based shape. Pearl th-49b4aa is now
  complete.

- 4cf018e: bench: permission-flow scenarios + headless `--auto-approve` flag.
  Closes the auto-mode work queue. `scenario.toml` gains an
  `auto_approve` meta field (default: `deny`) and a new
  `kind = "permission"` assertion that pins the expected resource +
  resolution scope. `th code --headless --auto-approve <mode>`
  spawns a tokio task that polls `/api/access/pending` and resolves
  each Ask per the configured mode — unattended runs are safe by
  default (every Ask becomes a deny) but can opt into permissive
  modes for bench scenarios that need them. 11 new tests across
  `scenarios::AutoApprove` parse/serde/round-trip + `auto_approve`
  module (fake-Big-Smooth integration for each mode, sentinel-drop
  stops the loop). Pearl th-400773.
- 04cdd6f: creds: credential helper broker — Docker-spec stdin/stdout binary +
  `/api/creds/issue` route. Sandbox tools that need authentication
  (git clone over HTTPS, gh CLI) get short-lived credentials minted
  by Big Smooth after a human approves the issue, instead of either
  shipping a long-lived PAT into the VM or denying the call. v1
  supports `github.com` via the host's `gh auth token`; AWS / Docker
  Hub / generic username/password are separate pearls.

  Flow:

  - `smooth-credential-helper get` reads `{ServerURL: ...}` from stdin
  - POSTs to `/api/creds/issue` on Big Smooth
  - BS checks wonk-allow.toml first (fast path); else files an
    AccessStore Ask
  - On approve at user/project scope, the host gets persisted to
    wonk-allow.toml so future mints skip the prompt
  - BS mints by calling the host's `gh auth token` (resolved against
    the same richer PATH `host_tool` uses, so it works under launchd)
  - Returns `{Username: "x-access-token", Secret: "ghs_..."}` to the
    helper, helper writes it back to git's credential framework

  19 new tests: 9 unit (backend selection, host extraction, scope
  serde, error display, mint error path), 4 helper bin (protocol
  PascalCase, IssueBody omits-empty, NO_CREDS git-compat string),
  6 integration (empty server → 400, pre-approved fast-path skips
  pending, human approve → 200, human deny → 403, pick_backend
  github subdomains, Ask shape carries kind=creds + full URL).

  Pearl th-08b65f. Mounting the helper inside the sandbox image
  (symlink at /usr/local/bin/git-credential-smooth, `git config
--global credential.helper smooth`) lands in a follow-up pearl —
  the broker + binary protocol are the core that future scopes (AWS
  STS, npm, Docker Hub) plug into.

- 9d04c6f: wonk/narc: ground the Claude-Code-style auto-mode permission model.
  `smooth_narc::judge::Decision` gains a fourth `Ask` variant with a new
  `scope_options: Vec<Scope>` field on `JudgeDecision` carrying the
  ladder (`Once` / `Session` / `PearlProject` / `User`) that the UI may
  offer the human. Legacy `EscalateToHuman` remains as the no-hint
  fail-closed form. New `smooth_bigsmooth::access::AccessStore` holds
  pending requests, broadcasts `AccessEvent`s for SSE consumers, and
  hands the caller a future that fires when a human resolves the
  request. Pearl th-49b4aa (Phase A) — TUI wiring + HTTP routes land in
  the dependent pearls.
- f4b1511: dispatch: non-sandbox path now gets Wonk/Narc parity. The "direct"
  dispatch path (no microVM) spawns operator-runner natively; the
  runner already brings up its own in-process Wonk via `spawn_cast`,
  but the spawn never received `SMOOTH_NARC_URL`. Result: the
  in-runner Wonk had no arbiter, hard-denied anything its local
  policy couldn't auto-approve, and the agent never reached the
  Claude-Code-style auto-mode prompts. Setting `SMOOTH_NARC_URL` on
  the direct-dispatch subprocess wires the runner's Wonk to Big
  Smooth's Safehouse Narc, so the same Decision::Ask → AccessStore
  → TUI → resolve loop now gates direct tool calls too. Pearl
  th-e96aeb.
- 442da1e: tests: add `sandbox_security.rs` integration suite exercising the
  Decision::Ask → AccessStore → human resolution → SafehouseNarc replay
  chain end-to-end. Covers: unknown domain holds-for-approve and
  holds-for-deny, dangerous CLI patterns refused by the rule engine
  before the Ask path runs, dangerous domains likewise, persistent
  wonk-allow.toml grants short-circuiting without prompts, glob
  matching against subdomains (and the adjacent-label safety guard),
  rule-engine safe domains, decision cache dedup, hold timeout failing
  closed, concurrent pending requests resolving independently, runtime
  merge_in taking effect without a Narc restart, glob_override flowing
  back through the resolution. 12 tests, in-process — the real-microVM
  gold standard from th-9dcc40's description is still on deck but
  needs a separate fixture investment. Pearl th-9dcc40.
- 50b1851: TUI: inline Claude-Code-style approval cards for Wonk Ask verdicts.
  The TUI subscribes to `/api/access/stream` and renders pending
  requests as compact cards under the chat scroll. Keystrokes
  `o`/`s`/`p`/`u`/`d`/`D` resolve the most recently filed open
  prompt at the chosen scope (once/session/project/user/deny-once/
  deny-forever) and POST to `/api/access/{approve,deny}`. Reconnects
  the SSE stream automatically with exponential backoff so a Big
  Smooth restart doesn't strand prompts. Pearl th-670fb2.

  Wire types moved to `smooth-narc::access_wire` so the TUI consumes
  them without taking a direct dep on smooth-bigsmooth; the orchestrator
  crate re-exports the same types so existing call sites compile
  unchanged. `AccessStore::subscriber_count()` lets integration tests
  wait for the broadcast subscription to register before firing events.

- dbc713a: tools: native `web_search` backed by DuckDuckGo HTML, no API key. New
  `smooth_bigsmooth::web_search` module + `GET /api/web_search?q=&n=`
  route. Big Smooth makes the outbound request so each sandbox doesn't
  need a TLS HTTP client + outbound permission for the search backend.
  `html.duckduckgo.com` and `duckduckgo.com` join the Narc obviously-
  safe domain list so the in-VM Wonk auto-approves without a human
  prompt. Untrusted result content is scanned for prompt-injection
  markers (`ignore previous instructions`, `</system>`, etc.) and
  redacted before return; `redacted_count` in the response surfaces
  how many hits fired. 16 unit tests (parser + redaction) + 8 wire-
  shape integration tests. Pearl th-70b68b.
- d37ce4d: wonk: persistent permission grants via `wonk-allow.toml`. Approvals
  at scope `user` (and for now, `project`) survive a Big Smooth
  restart — the resolution is appended to `~/.smooth/wonk-allow.toml`
  and Safehouse Narc consults the file at startup so subsequent
  requests for the same resource short-circuit to Approve without
  re-asking the human.

  Schema (v1): `[network] allow_hosts`, `[tools] allow`, `[bash]
allow_patterns`. Host patterns support `*.example.com` and
  `.example.com` glob shapes; bare suffixes require exact match (so
  `evil-example.com` can't slip past an `example.com` allow entry).
  Atomic writes via tempfile + rename. Pearl th-38b72c.

## 0.13.6

### Patch Changes

- 747921a: Fix `host_tool` spawn under macOS launchd-managed Big Smooth: the
  inherited PATH was minimal and didn't include `/sbin` (ping, route)
  or Homebrew dirs, so `host_tool({tool: "ping", ...})` failed with
  `spawn failed: No such file or directory`. Now resolves the tool's
  absolute path against a richer search list (`/usr/local/bin`,
  `/opt/homebrew/bin`, `/usr/bin`, `/bin`, `/sbin`, `/usr/sbin`)
  before spawning; falls back to letting Command walk inherited PATH
  when nothing matches.
- 48b59c5: "ping" means `ping`, not curl-as-a-stand-in:

  - Add `ping`, `dig`, `nslookup`, `host` to the host_tool CLI
    allowlist (`crates/smooth-bigsmooth/src/host_tools.rs`). All are
    reconnaissance-only, no host-state mutation.
  - Tool hints reorganized: separate `intent = "ping a host"` (ICMP)
    from `intent = "check if a host is reachable on a port"` (HTTP),
    plus a new `resolve a hostname` hint for `dig`/`host`.
  - Fixer prompt explicit: don't conflate "curl failed on port 80"
    with "host down" — many hosts answer ICMP but don't run HTTP.
    If the user asked to "ping", actually run `ping`.

## 0.13.5

### Patch Changes

- fade232: Make `host_tool` actually reach Big Smooth from inside a microsandbox
  VM on the 0.3.14 version we're pinned at. `host.containers.internal`
  has no DNS entry on 0.3.14 (that's a 0.4+ feature), and `127.0.0.1`
  from inside the guest routes via the guest's own loopback — never
  reaching the host-side TCP proxy. SMOOTH_NARC_URL now uses a
  routable host IP detected via the UDP-connect-to-public-IP-and-read-
  local-addr trick. Big Smooth listens on `0.0.0.0:4400`, so any of
  the host's real interface IPs lands on the listener; microsandbox's
  proxy `TcpStream::connect()`s the destination IP as-is. RFC1918
  addresses pass `NetworkPolicy::allow_all()` (which
  `allow_host_loopback: true` already enables). `SMOOTH_NARC_URL`
  env override still wins.

## 0.13.4

### Patch Changes

- 7d1497a: Fix `host_tool` connectivity from inside the sandbox: the runner now
  reaches Big Smooth's `/api/host/exec` at
  `http://host.containers.internal:4400` in both safehouse and
  host-modes. Previous code fell back to `http://127.0.0.1:4400` in
  host-mode, which inside the microsandbox VM means the SANDBOX'S
  loopback — not the host's — so every `host_tool` call failed with
  "error sending request for url (http://127.0.0.1:4400/api/host/exec)".
  The safehouse/host-mode distinction was a red herring; microsandbox
  exposes the outer host under `host.containers.internal` in both
  modes. `SMOOTH_NARC_URL` env override still wins.

## 0.13.3

### Patch Changes

- 1c7cc51: Add `host_tool` and `tool_hints` to the bigsmooth policy generator's
  `registered_tool_names()`. The previous fix only touched the runner's
  fallback `default_policy_toml()`, but Big Smooth's dispatch generates
  the actual policy that Wonk enforces — and that list was missing
  both tools. `host_tool({tool:"curl",args:["http://smoo-hub"]})` was
  still being denied with `host_tool is not in the tool allowlist`
  despite the runner having registered it.

  Sync test updated to pin the new entries so the two lists can't
  drift again.

## 0.13.2

### Patch Changes

- f231724: Make internal/Tailscale hostnames reachable from the sandbox via
  `host_tool`:

  - Add `host_tool` and `tool_hints` to the runner's default policy
    allowlist. `host_tool` is conditionally registered (only when
    `SMOOTH_HOST_TOKEN` is set during sandbox dispatch); listing it in
    the default `[tools].allow` lets the agent actually call it. Wonk
    still gates the underlying CLI choice on the host side via the
    separate host-tools allowlist (`gh`, `git`, `kubectl`, `jq`,
    `curl`).
  - Add a `check if a host is reachable` tool hint pointing at
    `host_tool({tool: "curl", …})` with the right `-fsS -o /dev/null
-w '%{http_code}'` template, plus an explicit note that there is
    no `http_fetch` tool — anyone reaching for one is hallucinating.
  - Add a "Hostnames, 'ping', and 'is X up?'" section to the fixer
    prompt telling the agent to take bare hostnames literally (no
    `.com` guessing), explaining why `bash ping` fails inside the
    sandbox (no Tailscale, no ICMP), and pointing at host_tool as the
    canonical path for internal hosts.

  Combined, "can you ping smoo-hub" now reaches `host_tool({tool:
"curl", args: ["http://smoo-hub"]})` instead of either denied
  `http_fetch` calls or `bash ping smoo-hub.com` chasing the wrong
  TLD.

## 0.13.1

### Patch Changes

- beb5596: TUI quality-of-life + agent context handling:

  - `th --resume` / `th --list` / `th --agent` work at the top level (no
    need to type `th code --resume`). Pearl th-resume-top-level.
  - Terminal resize no longer leaves duplicated tool-call rows above the
    inline viewport — `Event::Resize` clears the old viewport area before
    the next draw repaints. Pearl th-f294fd.
  - Agents no longer reply "I don't have context about what 'that'
    refers to" when the user uses a pronoun pointing at the prior turn.
    Two fixes: prompt guidance in fixer/oracle that names the pronoun
    patterns and the recovery path, plus a runner-side sanitizer that
    replaces malformed `<function=…>` / `<tool_call>` pseudo-XML in
    prior history with a clear `[NOTE: …did NOT execute]` marker so
    the model reasons about its own past attempt instead of staring at
    unparseable XML. Pearls th-c366ff, th-c65ca3.

## 0.13.0

### Minor Changes

- 175e60d: Cleanup batch: empty-arg normalization, oracle prompt tightening, real args/result on tool-call events

  Three pearls bundled (`th-75c3e5`, `th-962395`, `th-7a5106`):

  **Empty-args normalization (`th-75c3e5`)**
  Some small models (Gemini Flash family especially) emit a literal
  `""` empty-string for tools that take no parameters, instead of
  the schema-correct `{}`. Downstream hooks + tools that expect an
  object then fail on what should have been a no-op call. Fix in
  `smooth-operator::ToolRegistry::execute` + `execute_single`:
  normalize `Value::String("")` and `Value::Null` args to `{}`
  before any hook runs. `project_inspect("")` now succeeds.

  **Oracle prompt: don't bail after one tool error (`th-962395`)**
  Symptom: oracle would call `project_inspect`, get a tool error,
  then declare "I'm unable to list files as well" without ever
  calling `list_files` (which is in its allowlist). Prompt now has
  a "When a tool errors, try a different one" section that
  explicitly says: a single error doesn't mean the tool is
  unavailable; the system-prompt allowlist is the truth; pivot to
  the next sensible tool. Lists concrete fallbacks for the common
  cases (`project_inspect` → `list_files` + marker reads, `read_file`
  404 → next likely path, `grep` empty → broaden / `glob`).

  **Real arguments + result on tool-call events (`th-7a5106`)**
  `AgentEvent::ToolCallStart` only had `iteration` + `tool_name`;
  `ToolCallComplete` had `iteration` + `tool_name` + `is_error`.
  The full args / result / duration only flowed via the separate
  `ReporterEvent` HTTP channel — sandboxed dispatch, which parses
  the runner's stdout JSON-lines, ended up forwarding `arguments:
String::new()` and `result: String::new()` for every inner tool
  call. So inner `read_file` / `list_files` / `grep` calls
  rendered with empty args (or, in the user's experience, didn't
  render at all because the empty preview made them indistinguishable
  from each other).

  Adds:

  - `AgentEvent::ToolCallStart::arguments: String` (default `""`
    for backward-compat).
  - `AgentEvent::ToolCallComplete::result: String` + `duration_ms:
u64` (default `""` and `0`).
  - All emit sites populate the new fields.
  - Big Smooth's stdout parser (`server.rs`) reads them and forwards
    in `ServerEvent` instead of empty strings.
  - The TUI's `run_agent_streaming` already uses these fields — they
    just have real values now, so inner `read_file` / `list_files` /
    `grep` calls render inline with proper args + duration + result
    preview.

- 230ba6c: Per-workspace agent memory at `.smooth/MEMORY.md` (cold-start orientation)

  Cold-start agents land in `/workspace` with **zero idea what the
  project is**. The runner used to load `AGENTS.md` (if present) into
  the system prompt and that was it. For a fresh repo without
  AGENTS.md, the agent had no signal — it would guess from the
  question's phrasing and hallucinate ("you mentioned dev server,
  must be Rust") when the repo turned out to be Next.js.

  Adds a writeable per-workspace memory layer the agent maintains
  itself across sessions:

  - **`.smooth/MEMORY.md`** — auto-loaded into the operator system
    prompt at startup as a `## Workspace Memory` section, alongside
    the existing `## Project Context` from AGENTS.md. Empty / missing
    is fine; the system prompt tells the agent to populate it.
  - **`read_memory` tool** — returns the current contents (empty
    string when the file doesn't exist yet). Always cheap; intended
    to be called before answering any project-specific question.
  - **`write_memory` tool** — `mode='append'` (default) adds a
    section to MEMORY.md separated by a blank line; `mode='replace'`
    overwrites the entire file. Both modes create `.smooth/` if
    missing.
  - **Allowed for all roles**, including the read-only ones (oracle,
    mapper, heckler, scout). Memory is metadata, not source code;
    persisting findings is part of being a good cohabitant. (Scout
    gets `read_memory` only — sidekicks return summaries, not
    durable journal entries.)
  - **System-prompt discipline** — new "Memory & orientation"
    section in `prompts/system.md` codifies the loop:
    1. Assess. Do I actually know what this project is?
    2. Check loaded context first — `## Workspace Memory`,
       `## Project Context`. No tool call needed.
    3. If gaps remain, explore — `list_files` + read marker
       files (`README.md`, `package.json`, `Cargo.toml`, etc.).
    4. Persist what you learned — `write_memory` with terse
       bullets so the next session inherits.
    5. Re-check periodically — on long tasks, every several
       iterations, ask "have I learned something durable?"

  Effect: a "how do I run dev mode here" question on a fresh repo
  now goes (1) `read_memory` → empty → (2) `list_files` →
  `read_file package.json` → (3) answer + `write_memory` with the
  findings. Next session, (1) `read_memory` returns the bullets and
  the agent can answer without exploring.

- 28bea04: TUI: render ANSI escape sequences as actual colors (not strip, not raw)

  Previous pearl `th-a14138` stripped ANSI codes from streaming text
  because the markdown renderer was leaving `[2m...[0m` as raw
  literals. User wanted the colors _kept_ — they're how the runner's
  tracing logs become readable (dim timestamps, green INFO, italic
  field names).

  Replace `crate::ansi::strip` with a real SGR parser:

  - `ansi::line_has_ansi(line) -> bool` — cheap pre-check.
  - `ansi::parse_line_to_spans(line) -> Vec<Span<'static>>` — walks
    the SGR codes and produces styled ratatui Spans. Handles ESC-
    prefixed and bare-bracket forms (sometimes the ESC byte is
    scrubbed in transit). Supports: 0 reset, 1 bold, 2 dim, 3
    italic, 4 underline, 9 strikethrough, 22/23/24/29 modifier
    clears, 30-37 fg, 39 default fg, 40-47 bg, 49 default bg,
    90-97 + 100-107 bright variants, 38;5;N + 48;5;N (256-color),
    38;2;R;G;B + 48;2;R;G;B (true color).
  - 10 unit tests including a real runner-stderr sample.

  Wire-in (`inline::message_lines`): when the assistant content
  contains `[runner stderr]`, split there. Render the prose prefix
  through markdown as today; render the stderr suffix line-by-line
  with `ansi::parse_line_to_spans`. Diagnostics now display with
  their original styling instead of raw escape codes or stripped
  plaintext.

  `AppState::append_stream_content` reverts to passing content
  through verbatim — the rendering layer owns ANSI handling now.

- 2553b60: th smooth TUI: inline viewport (Claude Code style) + borderless chat

  The chat TUI used to live entirely inside an alt-screen with a fixed
  `Paragraph` that scrolled an in-app message buffer. That setup
  disabled the terminal's native wheel-scroll, drag-select, search, and
  copy — every one of those had to be re-implemented inside the app, and
  none of them worked well. Switch to ratatui's `Viewport::Inline`:

  - The TUI owns only ~14 rows at the bottom of the terminal: the
    input box, status bar, and an optional preview area for the
    in-flight streaming assistant message.
  - Finalized chat messages flow into the **terminal's own scrollback**
    via `Frame::insert_before`. A new `committed_count` cursor on
    `AppState` tracks which messages have been pushed; each event-loop
    tick flushes any newly-finalized ones before drawing the viewport.
  - Native wheel scroll, drag-select, search, and copy all work as
    they would for any other terminal output. No in-app reimplementation.

  Side effects:

  - Alt-screen is gone. `SMOOTH_TUI_NO_ALT_SCREEN=1` is now a no-op
    (kept readable so it doesn't error on shells with the var set).
  - The chat panel border was already redundant once selection moved
    to the terminal; it's removed entirely. Role labels + blank-line
    spacing carry visual structure.
  - Sidebar (`Ctrl+B`) is dropped. It needs an inline-friendly redesign
    (slash commands like `/git`, `/files` are the obvious next step).
    The keybinding is intentionally left unbound rather than re-purposed.
  - New `crate::inline` module: `message_lines` (single-message →
    styled `Line`s, shared between viewport preview and `insert_before`
    flush), `flush_to_scrollback`, `viewport_preview_lines`,
    `compute_regions`. Tested.

  Trade-offs:

  - The streaming preview area is capped at viewport height − 4
    rows. If a single response is taller than that, the most recent
    rows stay visible during streaming; the full text lands in
    scrollback when streaming completes.
  - The fancy gradient SMOOTH wordmark welcome banner is no longer
    rendered (kept as `#[allow(dead_code)]` for a possible
    fixed-screen toggle later). The system "Welcome to Smooth" line
    remains.

- 89834f0: th smooth TUI: scroll, selection, markdown, and intent-aware dispatch

  Four fixes to the chat TUI. They all stemmed from the same session
  where "how do I run dev mode" caused the agent to write
  `DEV_MODE_GUIDE.md` files and report a fabricated `1 passed, 0 failed`.

  - **Drop `EnableMouseCapture`.** Mouse capture was on but the event
    loop had no `Event::Mouse` arm — wheel scroll was dead AND text
    selection was dead because capture stole the drag. The TUI doesn't
    consume mouse events, so dropping capture lets the terminal handle
    both natively.
  - **Render assistant messages as markdown.** A new
    `smooth-code::markdown` module walks `pulldown-cmark` events into
    styled ratatui `Line`s. Bold, italic, inline code, fenced code
    blocks, headings, lists, blockquotes. Streaming-friendly: an
    unterminated fence renders as in-progress code rather than as raw
    backticks.
  - **`/agent` and `/ask` commands.** `/ask` switches to the read-only
    `oracle` role for Q&A — denies `edit_file`/`write_file`/`bash` so
    the agent answers without modifying the workspace. `/agent <name>`
    switches to any built-in role. Both pin the role, disabling the
    intent classifier below.
  - **Intent-aware dispatch.** When the user hasn't pinned a role, every
    message routes through a new `intent_classifier` shadow role (Fast
    slot, Haiku-class) that emits `WORK` or `QUESTION`. Questions
    dispatch under `oracle`; work dispatches under `fixer`. A pattern
    fallback keeps dispatch alive when the LLM gateway is unreachable.
  - **Runner: gate coding workflow on the role.** The coding workflow
    forces a "run tests, iterate until green, report N passed/failed"
    loop. Running it under a non-Coding-slot role (oracle, mapper,
    heckler) was producing the hallucinated `1 passed, 0 failed` line.
    The workflow now only runs when `active_role.slot == Coding` AND
    `bash` is allowed.

- f2c2c6f: th smooth TUI: render unified diffs for edit_file / write_file / apply_patch

  Tool-call rendering used to show only `tool_name("...args preview...")
── done`, with the actual change buried in a collapsed-by-default
  output blob. Worse, tool calls weren't even being attached to the
  assistant message in the first place — `ServerEvent::ToolCallStart`
  and `ToolCallComplete` translated into stub `AgentEvent`s that the
  event handler dropped on the floor (`_ => {}`). So the chat showed a
  wall of streaming text, no separate tool-call indicators, and zero
  information about what was edited.

  Two changes:

  - **Plumb tool calls through to state.** `run_agent_streaming` now
    takes the AppState `Arc<Mutex<_>>` and mutates it directly when
    it sees `ServerEvent::ToolCallStart` / `ToolCallComplete`. Tool
    calls hang off the most recent assistant message; ordering is
    preserved per-tool-name via a small per-name pending queue
    (`HashMap<String, VecDeque>`). The streaming assistant message
    is created synchronously before the recv loop so fast-arriving
    tool starts have somewhere to land.
  - **`crate::tool_diff` module.** `pub fn render(tool_name, args)`
    returns `Option<Vec<Line<'static>>>`. Recognizes `edit_file`
    (uses `path` + `old_string` + `new_string`), `write_file`
    (renders the new content as all-`+`), and `apply_patch`
    (renders the provided patch verbatim with consistent styling).
    Uses the `similar` crate for unified-diff generation with a
    2-line context radius. Caps at 200 rendered lines per call —
    big diffs get an `… N more diff lines elided …` marker in the
    middle. 7 unit tests.
  - **`inline::message_lines`** now suppresses the noisy
    `("...args preview...")` payload + the collapse glyph on
    diff-rendered tool calls (the diff itself is the content), and
    appends the styled diff lines after the header.
  - **`ToolCallState::arguments_full: Option<Value>`** preserves
    the parsed arguments for the renderer to consume. Marked
    `#[serde(skip)]` so saved sessions don't bloat with full file
    contents on every edit. New `ToolCallState::from_raw(id, name,
arguments_json: &str)` constructor for the WS dispatch path.
  - **`AppState::start_streaming` is now idempotent** — eager
    synchronous call (in `run_agent_streaming`) plus the lazy call
    (in `handle_agent_event`) no longer produce a duplicate empty
    assistant message.

### Patch Changes

- cd8f7c5: Tighten SMOOTH banner gradient boundary + clear all build warnings

  - Banner boundary: switch the Smoo→th split from 3/4 to 17/25 so
    teal lands at the T's left edge (col 38 in the 55-char ANSI-Shadow
    banner) instead of bisecting the letter.
  - `smooth-operator` `Activity::Planning` / `Activity::Thinking` are
    deprecated aliases for `Activity::Reasoning`; the `mapper` and
    `oracle` lead roles still referenced the old names. Updated both
    - the slot-routing test that asserted on the deprecated variants.
  - `smooth-bigsmooth/server.rs`: `if let Some(ref diver_client) = diver`
    bound a name that wasn't used; switch to `diver.is_some()`. Comment
    notes the binding pattern to restore when a real Diver client call
    is wired.
  - `smooth-bigsmooth/server.rs`: dead `SharedNarcHook` struct +
    `ToolHook` impl removed (never constructed). Dropped the now-
    dangling `async_trait`, `smooth_narc::NarcHook`, and
    `smooth_operator::tool::{ToolCall, ToolHook, ToolResult}` imports.
  - `smooth-bigsmooth/server.rs`: dead `chat_system_prompt()`
    function removed (no callers).
  - `smooth-operator-runner/lsp.rs`: drop the deprecated
    `InitializeParams::root_uri` field; we already pass
    `workspace_folders`, which is the LSP 3.6+ replacement.

  Build finishes with zero warnings; 468 tests still pass.

- bea267d: TUI banner: match the actual SVG-defined brand gradient (3 stops + leading solid bands)

  The previous pearl swapped vertical for horizontal coloring but used
  a 2-stop linear gradient (orange → pink, teal → blue). The brand
  gradient in `crates/smooth-web/web/public/logo.svg` is richer:

  - **Smoo zone**: 30 % solid orange (`#f49f0a`), then orange → coral
    (`#fb7a4d`) up to 79 %, then coral → pink (`#ff6b6c`) to 100 %
  - **th zone**: 43 % solid teal (`#00a6a6`), then teal → blue
    (`#1238dd`) to 100 %

  `theme::smooth_banner_color` now mirrors those stops. Leading solid
  bands give the wordmark its hold-then-fade shape — without them the
  banner read as a flat rainbow.

- cd25325: TUI welcome banner: paint the SMOOTH wordmark with the brand gradient (Smoo orange→pink + th teal→blue)

  The welcome banner used `theme::gradient_row(i, total_rows)` which
  paints each pixel-row uniformly top-to-bottom (yellow→green). Doesn't
  match the brand pattern — `Smoo` is orange→pink, `th` is teal→blue
  (see `theme::smooth_wordmark()`).

  New `theme::smooth_banner_color(col, total)` returns the right color
  for column `col` of a `total`-wide rendering, with the 6-letter
  split mapped to a 2/3 column split (4 of 6 letters in the Smoo zone).
  The banner now styles each character independently, so the brand
  gradient runs HORIZONTALLY across the wordmark the way it reads
  everywhere else in the product.

- 0ab0c72: TUI banner: structural split into (smoo, th) chunks — no more boundary math

  Previous boundary-as-fraction approach (`smoo_end = total * 17/25`)
  was fragile because the ANSI-Shadow letter widths drift between
  rows — some rows would land the boundary inside a glyph, leaving
  teal artifacts on the 2nd O's right edge.

  Refactor:

  - `BANNER_ROWS: [(&str, &str); 6]` — each row is now an explicit
    `(smoo_chunk, th_chunk)` tuple, split at the actual letter
    boundary in source.
  - `theme::smoo_gradient_color(i, total)` — the orange→coral→pink
    3-stop gradient, applied across only the smoo chunk's own length.
  - `theme::th_gradient_color(i, total)` — the teal→blue 2-stop
    gradient, applied across only the th chunk's own length.
  - `theme::smooth_banner_color` removed (was the fraction-based
    helper).

  Each half's gradient now fills exactly its half — `Smoo` is solid
  orange→coral→pink, `th` is solid teal→blue, and the boundary lands
  where it's supposed to, on T's left edge, not partway through it.

- fb3e71f: bench: prefer `~/.smooth/dolt/` over repo-walked store

  The bench's `locate_pearl_store_dir()` walked up from `cwd` first, so a
  bench launched from `~/dev/smooai/smooth/` bound to
  `~/dev/smooai/smooth/.smooth/dolt/`. The daemon, however, runs from
  launchd at `$HOME` and creates pearls in `~/.smooth/dolt/`. The two
  stores never met — the heartbeat task wrote `[PROGRESS]` comments to
  the daemon's store while the bench polled an empty one, and the
  600 s `idle_grace` always fired.

  Resolution priority is now:

  1. `SMOOTH_BENCH_PEARL_STORE` (explicit override)
  2. `~/.smooth/dolt/` (the daemon's default — almost always correct)
  3. Walk up from `cwd` for `.smooth/dolt/` (kept as a fallback for
     bench runs that explicitly target a project store)

  Confirmed root cause via direct inspection of pearl `th-79c2d3` during
  take 5: the heartbeat had written 5 `[PROGRESS]` comments at 30 s
  intervals into the smooai project store, while the bench was polling
  the smooth project store and never saw them.

- aa0cb46: bench: raise default chat-driver timeouts and make them configurable

  The bench's chat-agent-driven path
  (`crates/smooth-bench/src/chat_driver.rs`) had two hardcoded 120 s
  timeouts that consistently scored real solves as FAIL:

  - The reqwest HTTP-client timeout on `POST /api/chat` (the dispatch
    call). On a cold daemon or first-task dispatch the chat-agent
    sometimes legitimately takes longer than 120 s to spawn the
    teammate and return the pearl id.
  - The `idle_grace` quiet-timeout in the comment-polling loop. When
    the teammate doesn't post a `[PROGRESS]` comment within 120 s of
    the last comment, the bench treats the pearl as done and runs the
    test against an unchanged workspace — scoring real in-flight
    solves as FAIL.

  Both are now env-configurable with raised defaults:

  | Env var                            | Default | Purpose                             |
  | ---------------------------------- | ------- | ----------------------------------- |
  | `SMOOTH_BENCH_CHAT_HTTP_TIMEOUT_S` | 300     | reqwest timeout on `POST /api/chat` |
  | `SMOOTH_BENCH_IDLE_GRACE_S`        | 600     | comment-polling quiet timeout       |

  The chat-driver also logs `bench: pearl <id> polling with idle_grace=Ns`
  on dispatch so the active value is visible in the run log without
  inspecting env.

  Empirical evidence from the 2026-05-02 tuning-batch run
  (`docs/bench-sessions/2026-05-02-tuning-batch.md`): solves of
  cpp/all-your-base (218 s), java/alphametics (231 s), python/book-store
  (165 s), and others were all timing out at the 120 s grace. With 600 s
  the harness will let those finish. 300 s is the wall-clock budget for
  the dispatch-side HTTP call (the operator can run for much longer
  after that point, polled via the pearl).

  One unit test (`env_secs_falls_back_when_unset_or_invalid`) covers the
  unset / garbage / valid-integer branches of the env-reader helper.

- 77e49e3: dispatch + bench: wire cost reporting through pearl comments

  The bench's chat-agent dispatch path returned `cost: 0.0` hardcoded —
  the chat-agent dispatched a teammate, returned text to the bench, and
  the dispatched teammate's actual LLM cost (tracked in
  `AgentEvent::Completed` and aggregated as `final_cost_usd` in
  `dispatch_ws_task_sandboxed`) never crossed back to the bench.

  Fix uses pearl comments as the bridge:

  - `dispatch_ws_task_sandboxed` posts
    `[METRICS] cost_usd={final_cost_usd:.6} iterations={agent_iterations}`
    to the pearl right before closing it on success. Comment is part of
    the pearl's history, so anyone polling the pearl (the bench, the TUI,
    future tooling) can read the dispatch's actual spend.
  - `chat_driver::extract_cost(&[PearlComment])` scans for the latest
    `[METRICS]` line, parses the `cost_usd=X` token, returns it. Falls
    back to `0.0` for pre-fix runs and runs that errored before
    Completed.

  Both early-return paths in the bench's polling loop now use
  `extract_cost(&comments)` instead of literal `0.0`. Score JSONs
  generated by the next bench will carry real cost numbers.

- e515287: dispatch: post `[IDLE]` comment when sandbox exec terminates abnormally

  `dispatch_ws_task_sandboxed` only closed the pearl on the success path
  (exit 0). On `exec_in_sandbox` `Err(_)` and on non-zero runner exit
  codes, the pearl stayed `in_progress` with no terminal comment, so any
  poller waiting on `[IDLE]` / status-Closed had to fall through to the
  quiescence-grace timeout (default 600 s in the bench harness) to
  realise the dispatch was over.

  Both error paths now post a `[IDLE]` comment before returning:

  - `Err(e)` on `exec_in_sandbox`: `[IDLE] sandbox exec failed: {e}`
  - non-zero runner exit: `[IDLE] sandboxed runner exited with code {code}`,
    and the pearl status reverts to `Open` so the orchestrator can
    re-dispatch (matching `revert_pearl_to_open` semantics from the
    Mode B retry path).

  The bench harness's pearl-comment-polling loop already keys on
  `[IDLE]` as one of its three completion signals (alongside
  `PearlStatus::Closed` and the quiescence grace), so this drops
  worst-case task-failure latency from ~10 minutes to immediate.

- 230e5bd: dispatch: pearl-comment heartbeat during sandbox exec

  `exec_in_sandbox` is a blocking call — the runner's
  `AgentEvent::ToolCallStart` events arrive in one batch when the run
  finishes, so the pearl's comment count stays flat for the entire
  sandbox lifetime. Any external poller that uses comment-growth as a
  liveness signal (notably the bench harness, see
  `SMOOTH_BENCH_IDLE_GRACE_S`) gives up well before the operator is
  genuinely done.

  `dispatch_ws_task_sandboxed` now spawns a heartbeat task that posts
  `[PROGRESS] sandbox running (Ns elapsed, heartbeat #N)` to the pearl
  every 30s while the exec is in flight. The task is aborted as soon as
  exec returns (success, error, or destroy path).

  Tunable via `SMOOTH_DISPATCH_HEARTBEAT_S`:

  - `0` — heartbeat disabled (useful for tests or when observing
    genuine quiescence is desired)
  - `30` (default) — 30s cadence
  - any positive integer — custom cadence

  Without this, today's bench-harness 600s `idle_grace` still
  double-times-out tasks that are mid-LLM-call when the exec_in_sandbox
  poll is silent for the whole window. With it, the pearl gets a fresh
  comment every 30s and the harness's grace timer keeps resetting until
  the run actually finishes.

- 6d820e6: scripts: `pnpm install:th` now builds the embedded web SPA first

  `th` embeds `crates/smooth-web/web/dist/` at compile time via
  rust-embed. The old `install:th` was just `cargo install --path
crates/smooth-cli --force` — if you forgot to run `pnpm build` in
  `crates/smooth-web/web` first, the new binary would silently ship
  a stale web bundle. Bitten by this twice in one session.

  Fix:

  - New `pnpm build:web` — `pnpm install` + `vite build` inside
    `crates/smooth-web/web`, runnable on its own.
  - `pnpm install:th` now chains `pnpm build:web && cargo install
...`. Adds ~2 seconds per install (vite build is fast); pays
    for itself the first time it prevents stale-bundle confusion.

- af91499: Cleanup: subagent_dispatch test, smooth-web auto-placeholder, /verbose hides per-line `[runner]` stderr too

  Three small fixes:

  - **subagent_dispatch test** — `fixer_role_dispatches_scout_and_only_final_summary_leaks` asserted `obj.len() == 3`, but `DispatchResult` now has 4 fields when `verified_paths` is non-empty (the trust-but-verify follow-up from C4 added that field with `skip_serializing_if = Vec::is_empty`, and the test scenario triggers a `src/` path mention). Replaced the strict count check with a positive assertion that the three required fields are present and a closed-set check that no unexpected fields appear.
  - **smooth-web build.rs placeholder** — fresh worktrees previously failed to compile until you manually ran `pnpm build:web` to populate `crates/smooth-web/web/dist/index.html` (rust-embed needs the directory to exist at macro-expansion time). New `build.rs` writes a tiny placeholder if `dist/index.html` is missing, so any cargo build / cargo test in a fresh worktree just works. The first real `vite build` overwrites it. The directory is git-ignored so the placeholder doesn't leak into commits.
  - **`/verbose` hides per-line `[runner]` stderr** — pearl `th-ef181a` introduced `/verbose` and hid content after the `[runner stderr]` marker. But the sandboxed dispatch path (`server.rs:2596`) forwards each runner stderr line as its own TokenDelta with prefix `[runner] ` (no separator marker), and those lines kept leaking into the assistant content even with verbose off. Render now filters lines whose first 9 chars are `[runner] `, `[runner stderr]`, or `[cast-summary]` when verbose is off. Verbose on shows everything as before.

- a1a1bbf: operator runner: stop the moment tests pass — no over-iteration

  Empirical discovery from the M-workstream slot sweep: `glm-5.1` solved
  python/affine-cipher correctly (16/16 tests pass) within ~5-10 minutes of
  dispatch but kept iterating for 33+ minutes before posting `[IDLE]`. Same
  pattern observed in take 7 with kimi-k2-thinking (25 min), and likely in
  every "real solve" of take 7. The model lands a working answer, the test
  suite exits 0, and then the model keeps editing — refining, re-verifying,
  documenting, "improving" — until the iteration cap or some long quiet
  finally fires.

  Root cause: the D1 system prompt's "Verify before claiming done" block
  told the model to verify but not to _stop verifying_ once green. Models
  respect that ambiguity by continuing.

  Fix: replace the soft "Only then declare complete" with an emphatic STOP
  rule. Bench tasks that previously took 25-30 min should now finish in
  the 2-5 min range — the time it actually takes the model to write a
  correct solution and run the suite once.

  Models are fine. The prompt was the bottleneck.

- a9a9c86: operator: include `name` field on tool-result messages (Gemini OpenAI-compat fix)

  Per customer-service-bot research (memory:
  `reference_litellm_native_passthrough.md`), Gemini's OpenAI-compat shim
  maps `role: tool` to a `functionResponse` block, which has a `name` field
  that's not optional. Smooth was sending tool-result messages without
  `name`, so any call routed through the OpenAI-compat layer to a Gemini
  upstream would either drop the result silently or 400 with "requires a
  tool name for each tool call response."

  Fix is two-part:

  - `Message::tool_result_named(call_id, name, content)` constructor that
    attaches the originating tool's name to a tool-role message. Old
    `tool_result` retained for legacy callers.
  - `ChatMessage` adds a `tool_name` field that serializes as JSON
    `"name"` with `skip_serializing_if = "Option::is_none"` — present when
    set, omitted otherwise so legacy serialization is byte-identical.

  The agent loop (`agent.rs`, all 3 tool-result push sites in `run()` and
  `run_with_channel()`) now uses `tool_result_named(&tool_call.id,
&tool_call.name, &result.content)`. We always know the originating
  tool's name at result time, so the named constructor is the right
  default everywhere going forward.

  OpenAI ignores the field. Anthropic uses `tool_use_id` pairing already
  and doesn't reject the extra field. Sending it always is the safest
  serialization across providers.

  One new unit test (`tool_result_named_carries_name_through_serialization`)
  covers both branches: named results emit `"name":"..."`, legacy results
  omit the field entirely.

- 2163037: th pearls push: auto-set-upstream, `--force`, actionable error on diverged remotes

  `th pearls push` exposed only the bare Dolt push, so first-time push
  to a fresh remote failed with `fatal: The current branch main has no
upstream branch` and the user had to drop into raw `smooth-dolt sql -q
"CALL dolt_push('-u', 'origin', 'main')"` to recover. This pearl
  files three sharp edges:

  - New `PushOpts { force, set_upstream }` on `SmoothDolt::push_with`,
    re-exported from the crate. The bare `push()` stays as a no-flag
    shorthand for callers that don't care.
  - `th pearls push --force` (`-f`) overrides remote history when the
    remote has only a stale `Initialize data repository` commit from a
    previous abandoned init. No more raw SQL detour.
  - Auto-retry with `set_upstream = true` when the first push fails
    with "no upstream branch". Users don't need to know the flag exists.
  - Friendlier error on "no common ancestor" — the bare Dolt message
    was unhelpful; now the CLI surfaces the two real recovery paths
    (force push, or `git push origin --delete refs/dolt/data` then
    push) with a one-liner inspection command for the curious.
  - Tightened `is_no_remote_error` so it only matches "no configured
    push/pull destination" — "no upstream" used to live there, which
    meant the global pearl store silently swallowed first-push errors
    instead of recovering with `-u`.

- 1263033: operator-runner: family-aware Anthropic shape for Claude models

  Probed the gateway: `https://llm.smoo.ai/v1/messages` (LiteLLM's
  Anthropic-shape route) already resolves smooth-\* aliases AND uses native
  Anthropic shape with proper `tool_use` / `tool_result` block pairing.
  The OpenAI-compat translation at `/v1/chat/completions` silently mangles
  Claude tool calls on the second turn (per customer-service-bot research,
  memory: `reference_litellm_native_passthrough.md`).

  Smooth's LLM client already supports `ApiFormat::Anthropic` and
  `convert_messages_to_anthropic` — they construct `<api_url>/messages`
  requests with the right shape. The gap was the operator-runner always
  selecting `OpenAiCompat` regardless of the routed model.

  Fix:

  - New `provider_overlay::is_anthropic_family(&str)` helper detects
    Claude-class models (smooth-judge, smooth-fast-haiku, smooth-reviewing-haiku
    aliases, plus any model name containing `claude`, `anthropic`, `haiku`,
    `sonnet`, `opus`). Case-insensitive.
  - Operator runner's LlmConfig construction site (line ~1559) now picks
    `ApiFormat::Anthropic` when the family check matches, otherwise the
    existing `OpenAiCompat`. Logs the routing decision via tracing for
    observability.

  Combined with the prior tool-name compat fix (PR before this), Smooth now
  routes:

  - Claude models → `https://llm.smoo.ai/v1/messages` (Anthropic-shape,
    alias-resolving)
  - Everything else → `https://llm.smoo.ai/v1/chat/completions` (OpenAI-compat,
    alias-resolving, with tool-result `name` field for Gemini compat)

  One unit test covers the family detection across alias and direct-model
  spellings + negative cases (gpt, kimi, gemini, deepseek must NOT match).

- e580a9f: build-operator-runner.sh + install:th: keep `~/.smooth/runner-bin/` in lockstep with `target/`

  Big Smooth's `find_operator_runner_binary` walks up from
  `CARGO_MANIFEST_DIR` looking for
  `target/aarch64-unknown-linux-musl/release/smooth-operator-runner`.
  A long-running `th up` daemon compiled in a worktree whose `target/`
  no longer holds the binary can fall back to a stale
  `~/.smooth/runner-bin/` copy left by an earlier setup. Net effect:
  fresh runner code (e.g. the `coding_workflow` role gate from
  `th-c1e2c0`) never reaches the sandbox even after rebuild +
  reinstall — sandbox runs old binary, oracle still gets shoved
  through fixer's coding workflow.

  Two fixes:

  - `scripts/build-operator-runner.sh` now copies the freshly-built
    binary into `~/.smooth/runner-bin/smooth-operator-runner` after
    every cross-compile. Both find paths resolve to fresh.
  - New `pnpm build:runner` script wraps the build script. `pnpm
install:th` chains `build:web && build:runner && cargo install`,
    so a single `pnpm install:th` now refreshes everything: web
    bundle, sandbox runner, and host `th` binary. The script's
    cross-compile is incremental — adds ~5s when no runner sources
    changed, ~30s when they did. Worth it to kill the stale-binary
    footgun.

- 08470fa: runner: single-agent path now resolves the LLM config from the active role's slot

  When the workflow gate skips `coding_workflow` (oracle / mapper /
  heckler — anything that isn't a Coding-slot lead with `bash`
  allowed), the runner falls through to the single-agent path. That
  path was building `LlmConfig` from the `SMOOTH_*` env vars, which
  big-smooth populates from the default provider's default model
  (`smooth-fast-gemini` in the canonical setup). Result: oracle
  (`slot = Reasoning`) was calling the **Fast** model instead of
  Reasoning — both wrong-tier-for-the-task and the very model that
  just hit a 503 on Vertex AI.

  After active-role resolution but before `agent_config` is built,
  re-parse the routing JSON (already mounted into the sandbox at
  `/opt/smooth/policy/routing.json`), then ask
  `ProviderRegistry::llm_config_for(active_role.slot)` for the
  right model. That config replaces the env-var default for the
  single-agent path. The workflow path is unaffected — it does its
  own per-phase resolution further down using the same registry.

  Falls back to the env-var default cleanly when the routing JSON
  is missing, unparseable, or the slot can't resolve — preserves
  existing behavior for tests and minimal setups.

- 7ae64f3: Status bar: show the resolved active model, not a hardcoded "claude-sonnet-4" default

  `AppState::new` defaulted `state.model_name` to `"claude-sonnet-4"`
  and the status bar printed it verbatim — never updated, so the
  label was wrong for any session running through Gemini, DeepSeek,
  or anything other than Claude.

  Status now derives the label live:

  - **In-flight**: prefer `current_phase_alias` (e.g. `smooth-reasoning`)
    with `current_phase_upstream` appended when known
    (`smooth-reasoning → claude-opus-4-5`). Both are populated by the
    runner's `PhaseStart` events.
  - **Idle**: synthesize the alias from the active role's slot —
    `smooth-{slot}` (`smooth-coding`, `smooth-reasoning`, etc.) —
    matching the convention in `~/.smooth/providers.json`.
  - **Unknown role** (typo / custom role): fall back to the role name.

  `state.model_name` is left in place since the model picker + session
  save path still use it; just no longer driving the status bar.

- 869e306: TUI: strip ANSI escape sequences from streaming assistant content

  The runner emits structured tracing logs colored with ANSI SGR codes
  (`\x1b[2m...\x1b[0m`, `\x1b[32m INFO`, `\x1b[3mfield\x1b[0m=value`,
  etc.). Big Smooth forwards runner stderr as `TokenDelta` chunks for
  the assistant message, those codes ride along, and the markdown
  renderer treats them as plain text — the chat fills with raw
  `[2m2026-05-07T13:43:52.300628Z[0m [32m INFO[0m ...` litter.

  New `crate::ansi::strip(s)` does a linear scan and removes any
  `\x1b[<digits>(;<digits>)*m` sequence, plus the bare-bracket form
  `[<digits>(;<digits>)*m` (the ESC byte is sometimes lost in transit
  through WebSockets / terminal copy-paste). Conservative — only
  matches digit-only param sequences ending in `m`, so legit
  markdown like `[link](url)` and array syntax `[1, 2, 3]` stay
  untouched. 8 unit tests including a real runner-stderr sample.

  Hook point: `AppState::append_stream_content` strips before
  pushing into the message buffer. Markdown render and
  `flush_to_scrollback` see clean text.

  (Web parity is filed separately as `th-a14138` — same fix needed
  in chat.tsx's WebSocket handler.)

- e65d0d6: th smooth TUI: surface coding-workflow activity inline

  Today the TUI forwards `TokenDelta`, tool calls, and the final
  `Completed`/`Error` events. Everything else the runner emits
  (iteration boundaries, snapshots, max-iter caps, budget breaches,
  Warn-level Narc alerts) was silently dropped. So a long workflow
  run looked like one streaming blob with no signal of what was
  actually happening.

  The 7-phase decomposition (ASSESS / PLAN / EXECUTE / VERIFY /
  REVIEW / FINALIZE) is gone — see
  `crates/smooth-operator/src/coding_workflow.rs:15`. Only the
  single `CODING` phase + an iteration counter remain. So the
  "phase breadcrumbs" idea collapses into "iteration breadcrumbs".

  `handle_agent_event` now surfaces:

  - `PhaseStart { iteration, alias }` → inline system line
    `→ iteration N • {alias}`. Lands once per outer iteration of
    the coding workflow so the user can see the workflow pacing.
  - `CheckpointSaved { iteration }` → muted line `✓ snapshot taken
(iter N)`. Confirms the best-seen-workspace snapshotting is
    doing its job.
  - `MaxIterationsReached { max }` → `⚠ hit max iterations (N) —
stopping`. Was previously dropped on the floor with no user-
    facing signal.
  - `BudgetExceeded { spent_usd, limit_usd }` → `⚠ budget exceeded
— spent $X of $Y`. Same — was silent.

  `ServerEvent::NarcAlert` handling is now severity-aware:

  - `Block` (the call was actually denied) → unchanged, surfaces
    as `Error` and terminates the run.
  - `Warn` (informational alert, did NOT block execution) → new
    inline system message `⚠ Narc Warn • {category}: {msg}`. The
    run keeps going; the user sees the warning. Previously every
    Warn was incorrectly routed as an Error and killed the
    response.
  - Anything else → quiet.

  The `category` field of NarcAlert is now plumbed through (was
  dropped via `..`) so the user knows whether the alert is
  about secrets, prompt injection, etc.

- 1280093: th smooth TUI: restore the gradient SMOOTH wordmark welcome banner

  The previous pearl (Viewport::Inline switch) marked the welcome
  banner as `#[allow(dead_code)]` because there was no longer an
  empty-state region inside the viewport to paint it in. Bring it
  back the inline-native way:

  - `render::welcome_banner_lines()` is the public builder — returns
    `Vec<Line<'static>>` for the gradient box-drawing wordmark + the
    "AI Agent Orchestration Platform" / "smoo.ai" / "type a message"
    tagline lines.
  - `app::run` calls `inline::insert_before_lines` once at session
    start (fresh sessions only — resumed sessions skip the banner)
    to push it into the terminal's scrollback BEFORE any chat
    messages. It sits at the top of the session like a real
    terminal program's startup banner, scrollable, selectable,
    copyable like any other terminal output.
  - The verbose "Welcome to Smooth. Type a message and press
    Enter to chat." system line is replaced by the shorter
    "Type a message to get started. /help for commands." (the
    banner already says the equivalent).

- 932f15c: th smooth TUI: fix intent classifier bypass + drop dead Ctrl+B hint

  The intent classifier (added to route questions to oracle and work
  to fixer) never fired for fresh `th` invocations — every session
  was silently pinned to fixer, so questions like "how do I run dev
  mode" still ended up in the coding workflow with file writes and
  hallucinated test counts.

  Root cause: `cmd_code` in `smooth-cli/src/main.rs:2204` always
  passed `Some(agent_name)` to `app::run_with_session`, where
  `agent_name` was unconditionally resolved to `"fixer"` via
  `resolve_primary_agent(None)`. `app::run` saw `Some(_)` and set
  `agent_pinned = true`, which bypassed the classifier branch in
  `handle_input_mode`.

  Fix: pass the **original** `agent: Option<String>` (the unresolved
  CLI flag) to `run_with_session`. `agent_name` stays around for the
  typo-validation call and the headless path. Now when the user
  runs plain `th` (no `--agent`), `agent` is `None`,
  `agent_pinned` stays `false`, and the classifier runs per
  message. Explicit `--agent foo` still pins as designed.

  Bundled cleanup: dropped the dead `Ctrl+B sidebar` hint from the
  status bar. The keybinding was removed when sidebar rendering went
  away in the inline-viewport pearl, but the status bar text never
  got updated.

- 3138cbc: C4: trust-but-verify on sidekick dispatch return

  `DispatchResult` (the JSON the parent agent gets back from a successful
  `send_sidekick` call) now carries two new fields:

  - `verified_paths: Vec<String>` — file paths the sidekick named in its
    summary that the parent confirmed exist on the host filesystem at
    dispatch return time (either as absolute paths or relative to CWD).
  - `unverified_paths: Vec<String>` — paths the parent couldn't verify;
    may have been renamed, moved, never existed, or be relative to a
    workspace the parent doesn't share.

  Both fields are `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
  so the JSON shape is unchanged for the common no-paths case (existing
  parent-side parsers don't break).

  Two new public free functions in `smooth-operator::cast::dispatch`:

  - `extract_claimed_paths(text)` — scans free text for tokens that look
    like file paths (contain `/`, or end with a known code/config
    extension), strips trailing punctuation, deduplicates.
  - `verify_paths(claimed)` — checks each claimed path against the host
    filesystem (`Path::exists()` as-given or under CWD), returning
    `(verified, unverified)`.

  `DispatchSubagentTool::execute()` runs both after the sidekick returns,
  so the parent's reasoning includes a structured trust-but-verify list
  without requiring any extra plumbing on the parent side.

  3 new unit tests (path extraction, dedup + prose-rejection,
  verify_paths classification). Existing
  `dispatch_result_serializes_to_expected_shape` extended to cover both
  the empty-paths case (3 visible JSON fields) and the populated case.

- fa2ca85: sandbox: pass `SMOOTH_API_KEY` as a plain env var (interim — secret substitution silently broken)

  Sandboxed dispatch wired `SMOOTH_API_KEY` through microsandbox's
  `SecretBuilder` with a placeholder + `allowed_hosts`, expecting the
  network layer to swap on outbound. Confirmed in
  `smooth-bootstrap-bill/server.rs` — `n.secret(...).env(...).value(...).
placeholder(...).allow_host(...)`. But the runner's single-agent path
  makes a Bearer-auth request to `https://llm.smoo.ai/v1` and the literal
  `SMOOTH_PLACEHOLDER_API_KEY_NOT_SUBSTITUTED` reaches LiteLLM, which
  returns 401: "Authentication Error, LiteLLM Virtual Key expected.
  Received=SMOO\*\*\*\*UTED, expected to start with 'sk-'".

  Likely cause: microsandbox 0.3.14's `NetworkPolicy::allow_all()`
  (set when `allow_loopback=true`, which is the default for our
  sandbox config) bypasses the secret-substitution middleware. The
  two compose oddly. May be fixed in 0.4.x.

  Until that's investigated (parent pearl `th-6030b0`), bigsmooth
  injects the real API key directly via `env.insert("SMOOTH_API_KEY",
api_key)` and passes an empty `secrets: Vec::new()` to the sandbox
  config. Known regression: agents in the VM can read their own
  LLM API key (exfil risk via tool output, scraped logs, etc.). The
  runner still sends the same Bearer auth — LiteLLM now sees the
  real `sk-` key and accepts.

- 11271c0: TUI: hide `[runner stderr]` / `[cast-summary]` diagnostic block by default; toggle with `/verbose`

  Every assistant turn was dumping the runner's tracing logs +
  cast-summary JSON at the end of the message. Useful for debugging,
  but for the vast majority of turns it's just noise that buries the
  actual answer.

  Default to hidden:

  - New `AppState::verbose: bool` (default `false`).
  - New `/verbose` slash command — no-arg toggle, or explicit
    `/verbose on` / `/verbose off`.
  - `inline::message_lines_with_verbose(msg, verbose)` — same shape
    as `message_lines` but with explicit control. The default-export
    `message_lines` keeps `verbose=false` for callers that don't
    thread state. The active dispatch path (`flush_to_scrollback` +
    `viewport_preview_lines`) reads `state.verbose` and passes
    through.
  - Content stays in `msg.content` either way, so saved sessions
    round-trip correctly — only the rendered output skips the
    diagnostic block when verbose is off.

- 0203d15: smooth-web: surface coding-workflow activity inline (parity with TUI's th-c83d13)

  The `/ws` endpoint already broadcasts every `ServerEvent` — but the
  chat page only filtered for `BigSmoothThought` to drive the floating
  bubbles next to Big Smooth's face. Iteration boundaries, snapshot
  saves, max-iter caps, budget breaches, and Narc warnings were
  silently dropped on the web client (the TUI got them in pearl
  `th-c83d13`).

  Frontend-only change (backend already emits everything):

  - `Msg` interface gains a third role `'activity'` for ephemeral
    status breadcrumbs. They live only in the live-session
    `messages` state — a page refresh drops them since
    `ChatMessageView` doesn't persist them.
  - `chat.tsx`'s `/ws` `onmessage` handler now branches on:
    - `PhaseStart { iteration, alias }` → `→ iteration N • {alias}`
    - `CheckpointSaved { iteration }` → `✓ snapshot taken (iter N)`
    - `MaxIterationsReached { max }` → `⚠ hit max iterations (N) — stopping`
    - `BudgetExceeded { spent_usd, limit_usd }` → `⚠ budget exceeded — spent $X of $Y`
    - `NarcAlert` (`severity === 'Warn'` only) → `⚠ Narc Warn • {category}: {msg}`
  - Activity messages gate on a `streamingRef` so a background
    dispatch for another session doesn't pollute the current view.
    `/ws` is broadcast across all sessions and `ServerEvent`s don't
    carry session ids today.
  - The renderer treats `role: 'activity'` distinctly: thin
    monospaced one-line, muted by default, amber when the line
    starts with `⚠`. No avatar, no role label — they should feel
    like terminal status lines, not chat bubbles.
  - Block-severity Narc alerts are unchanged; they still flow
    through the regular error path so we don't double-render.

  Floating thought bubbles (the Fast-slot first-person summarizer)
  are unaffected — additive, not in conflict.

## 0.12.11

### Patch Changes

- 993f3f9: D6: intent-based memory typing + verify-before-recommend rule on recall

  Adds four intent-based variants to `MemoryType` adapted from the Claude
  Code v2.1.120 memory subsystem:

  - `User` — durable facts about the user (role, expertise, preferences).
  - `Feedback` — corrections or validations on approach. Highest leverage
    type — re-reading prevents re-litigating decisions.
  - `Project` — current state of in-flight work (initiatives, deadlines,
    who's doing what). Decays fast.
  - `Reference` — pointers to where information lives outside this
    project (Linear, Slack, Grafana, etc.).

  The original scope-based variants (`ShortTerm`, `LongTerm`, `Entity`)
  are preserved unchanged for backward compatibility.

  `MemoryType::needs_freshness_check()` flags `Project` and `Reference`
  as time-sensitive. The agent-context builder
  (`Agent::build_context_messages`) checks recalled entries and, when any
  need a freshness check, prepends a verify-before-recommend note to the
  recalled-memories block:

  > Note: 'the memory says X exists' is not the same as 'X exists now'.
  > Before recommending or acting on any function path, file, flag, or
  > external pointer named below, verify it's current by reading the file
  > or grepping the codebase. Project and Reference memories are
  > time-sensitive; User and Feedback are durable.

  This is the structural counterpart to the recall-discipline auto-memory
  rules: now the runtime nudges the model to verify, not just the prompt.

  2 unit tests (variant serialization round-trip; freshness-check guard).

## 0.12.10

### Patch Changes

- a2982d7: C1: pre-filter the operator-runner's tool registry by the active role's clearance

  The runner registers ~20 tools (file/bash/lsp/bg/network/etc.) and then
  adds a `PermissionHook` that rejects calls to tools the active role isn't
  allowed to use. That keeps the user safe but wastes a turn each time the
  LLM calls a denied tool — the model picks the tool from the schema set,
  gets a permission error, and has to retry.

  Now the runner runs `tools.retain(|name| active_role.permissions.allows(name))`
  before installing hooks, so denied tools are gone from the schema set the
  LLM ever sees. PermissionHook stays as second-line defense in case a tool
  is registered later in the lifecycle.

  Adds `ToolRegistry::retain<F: Fn(&str) -> bool>` in
  `crates/smooth-operator/src/tool.rs` so other call sites can do the same
  filter without scraping `tools` directly.

  One unit test (`retain_drops_unallowed_tools_only`) confirms the filter
  drops disallowed tools while keeping hooks intact.

- ef6c669: Per-provider operator-runner system-prompt overlays (opencode pattern)

  The operator runner now prepends a short, model-family-specific overlay to
  the base `system.md` before dispatching the LLM call. Adapted from
  opencode's per-provider prompt directory (`anthropic.txt`, `beast.txt`,
  `gemini.txt`, `kimi.txt`, …) but trimmed and re-tuned for the Smoo cast
  vocabulary.

  7 overlay files added at `crates/smooth-operator-runner/prompts/providers/`:

  - `anthropic.md` — Claude family. Lean into long-form reasoning + tool
    precision; restraint rules apply _especially_ hard since the family
    trends toward over-explaining.
  - `gpt.md` — GPT/Codex/o-series. The big counter-failure-mode block:
    "keep going until completely resolved", "training data is out of date",
    "no half-finished implementations", "verify before claiming done."
  - `gemini.md` — Gemini family. Native tool calls (no `tool_code` blocks)
    - long-window drift mitigation (re-read after each meaningful change).
  - `kimi.md` — MiniMax / Kimi / `smooth-coding` default. Bias to action,
    smallest correct edit, build-then-claim-done.
  - `deepseek.md` — `smooth-reasoning` slot. Plan-then-act, but reasoning
    isn't an excuse to skip verification.
  - `glm.md` — Z.ai / GLM. Tool-call format precision, no over-elaborate
    preambles.
  - `qwen.md` — Qwen. English-only output in code; native tool-call schema.

  `crates/smooth-operator-runner/src/provider_overlay.rs` adds the loader:

  - `for_model(&str) -> Option<&'static str>` returns the right overlay
    given a model identifier.
  - Smoo semantic aliases resolve first (`smooth-coding` → kimi,
    `smooth-reasoning` → deepseek, `smooth-fast-gemini` → gemini,
    `smooth-judge` → anthropic, etc.) — pinned so a gateway routing flip
    doesn't silently change the prompt scaffold.
  - Family substring fallback handles explicit model strings like
    `claude-haiku-4-5-20251001`, `kimi-k2-thinking`, `gpt-5.4-mini`,
    `gemini-3-flash`, `deepseek-v3.2-speciale`, `glm-5.1`, `qwen3-coder-plus`.
  - Unknown models return `None` and the runner falls back to the base
    prompt unchanged — non-breaking for any unconfigured model.

  `main.rs` system-prompt assembly prepends `provider_overlay::for_model(...)`
  output before `system.md`. 5 unit tests cover alias resolution, family
  substring matching, prefix-order safety (smooth-fast-gemini must hit gemini
  not gpt), unknown-model fall-through, and overlay-content non-emptiness.

  This is the prompt-side complement to the routing slot work — when the
  gateway routes coding to Kimi, the runner now boots with Kimi-tuned
  discipline rules rather than the generic base prompt.

## 0.12.9

### Patch Changes

- 9bbda0a: Enforce `context_brief` on `teammate_spawn` and inject a max-steps reminder
  on the agent loop's final iteration

  ### C3 — `context_brief` is now a structurally-required field

  `crates/smooth-bigsmooth/src/chat_tools.rs` `TeammateSpawnTool`:

  - Adds `context_brief` as a required tool-schema field with `minLength: 80`.
  - `execute()` rejects any call where the trimmed brief is under 80 chars
    with a teaching error message that lists what a real briefing covers
    (what you've learned, what you've ruled out, files/paths/commands to
    start with, judgment dimensions to flag back) and tells the model to
    re-issue rather than just retry.
  - The teammate's task message is now structured: pearl description →
    `## Context from team lead` → context_brief → optional
    `## Extra constraints` → extra_prompt. Teammates get a clear scaffold
    instead of one big concatenated blob.

  This is the structural enforcement of the prompt-side rule landed in the
  D2 batch: previously the chat agent could ignore the rule; now the
  runner rejects the call and forces a recovery turn.

  ### D4 — max-steps reminder on final iteration

  `crates/smooth-operator/src/agent.rs`:

  - New `MAX_STEPS_REMINDER` constant (adapted from opencode's
    `max-steps.txt` — opencode's tool-disabling reminder works because
    it instructs a clean wrap-up rather than reading like an error).
  - Both `run()` and `run_with_channel()` push the reminder as a system
    message on the final iteration before the LLM call. The model sees
    "this is your final iteration; respond with text only — what's done,
    what's left, what to recommend next" and writes a useful summary turn
    instead of starting a tool chain that gets cut off.

  ### Tests

  - 3 new chat_tools unit tests (threshold range, rejection-message scaffold,
    schema-name stability)
  - 1 new agent test (`max_steps_reminder_includes_recovery_scaffold`)
  - All existing tests still pass

  Together with D1+D2 (prompt rewrites) this completes the prompt-and-loop
  half of the typed-sniffing-badger Pillar D plan. C1 (per-role tool
  clearance enforcement at the runtime layer) and C4 (trust-but-verify
  hook) remain for follow-up work.

## 0.12.8

### Patch Changes

- d9841fc: Adopt Claude Code v2.1.120 + opencode tuning patterns in the operator runner and Big Smooth chat-tools prompts

  Operator runner system prompt (`crates/smooth-operator-runner/prompts/system.md`)
  fully rewritten around five high-leverage discipline blocks lifted from the
  Claude Code v2.1.120 prompt and opencode's `anthropic.txt`:

  - Restraint: no premature abstraction (three similar lines beats one), no
    validation for can't-happen scenarios, no comments by default — only WHY
    when non-obvious, never WHAT or task references that rot.
  - Verify before claiming done: type-check + tests must pass; "code correctness
    is not feature correctness; if you can't exercise the feature, say so."
  - Blast radius / reversibility: explicit destructive-op list (rm -rf, git
    reset --hard, force push, package downgrade, CI/CD edits, sending messages)
    each requiring scope-bounded authorization. "Authorization stands for the
    scope specified, not beyond."
  - Communication discipline: one sentence before first tool call, short
    updates at find/pivot/blocker, two-sentence end-of-turn summary, no colons
    before tool calls.
  - Loop hygiene: don't retry failing commands in sleep loops; diagnose root
    cause; don't repeat a rejected call.

  Existing Smooth-specific operational guidance (project_inspect, lsp,
  edit_file/write_file/apply_patch, bg_run, mise) preserved and trimmed.

  Big Smooth chat-tools prompt (`crates/smooth-bigsmooth/src/chat_tools_system_prompt.txt`)
  gets three additions on the same theme:

  - `teammate_spawn` rule 3 now requires a `context_brief` — "brief the teammate
    like a smart colleague who just walked into the room: what you've learned,
    what you've ruled out, files to look at, judgment-call dimensions to flag.
    Never delegate understanding."
  - New "trust but verify" line on the workflow: spot-check teammate output by
    reading a file or running the build before reporting work as done.
  - Style block extended with the same one-sentence-before / no-colon /
    exploratory-question discipline rules.

  Both files compile via `include_str!` with no Rust changes needed; the
  operator-runner binary will pick up the new prompt on next rebuild via
  `scripts/build-operator-runner.sh`.

## 0.12.7

### Patch Changes

- 866eeaf: Pearls in project repos sync via git

  `.dolt/` was globally gitignored, which meant project pearl boards
  (`<repo>/.smooth/dolt/.dolt/`) were excluded too — no cross-machine
  sync. Anchored the ignore to the repo root so legacy top-level
  `.dolt/` stores still stay out, and added `.smooth/dolt/.gitignore`
  that scopes runtime files (LOCK, temptf/, stats/) inside the pearl
  store while letting the manifest + content-addressed blobs ride
  along with the project's git history.

  Workflow: `th pearls create` → blobs written → `git add .smooth/dolt`
  → `git commit` → `git push`. Other machine: `git pull` and
  `th pearls list` shows the same board.

  Trade-off: dolt blobs grow git history. Acceptable for personal +
  small-team boards; revisit if a board's churn becomes painful.

  Long-term: a real Dolt remote (DoltHub or self-hosted SQL server
  on tailnet) is a cleaner solution; tracked in `th-94f6b6`. This
  gitignore fix is the immediate "pearls sync between machines now"
  unblocker.

## 0.12.6

### Patch Changes

- ba63393: Stronger chat-agent prompt — clone goes through bash, not a teammate

  Even after adding the bash carve-out for one-shot writes, the chat
  agent kept reaching for `teammate_spawn` on `git clone` requests
  because the rule was buried mid-prompt. Reorganized the prompt around
  a numbered "decision rules" block at the top with rule 1 being
  "clone/fetch/mkdir → bash, NOT teammate_spawn" — explicit, ordered,
  non-negotiable.

  Also tightened `teammate_spawn`'s tool description:

  - Lead sentence is now "for REAL CODING WORK ... do NOT use this for
    one-shot bash-allowlist commands". Models are likelier to skip a
    tool whose schema says "don't use for X" than to read past five
    paragraphs to find the same caveat.
  - The `model` parameter description explicitly warns against
    `smooth-fast-gemini` (it can't reliably emit native tool calls and
    wedges the runner) and removes the prior advice to use it for
    read-only lookups, which was the trigger for the 5-min wedge this
    morning.
  - The `working_dir` field's description explicitly says "never pass a
    directory as broad as ~ or /". The wedge happened with
    `working_dir=/Users/brentrager`.

  Verified end-to-end: `clone brentrager/budgeting to
~/dev/brentrager/budgeting` now answers in ~47 s with the repo
  actually cloned (verified via `ls .git` on the destination).

- 70244a9: Fix `th pearls create` silently dropping writes from CLI mode

  `smooth-dolt sql -q ...` ran every statement through Go's
  `db.Query`, including writes (INSERT/UPDATE/DELETE). Dolt returns
  `__ok_result__` for those, but the implicit transaction never
  commits to the working set before the subprocess exits — Dolt
  rolls it back. Result: `th pearls create`'s INSERT was silently
  dropped, then `store.create`'s verify-after-create failed with
  `pearl not found after create: th-XXXXXX` and the row was gone
  from disk.

  Server mode (`smooth-dolt serve`) had a separate `doExec`
  (uses `db.Exec`, commits on close); CLI mode had no equivalent.

  Fix:

  - New `smooth-dolt exec <data-dir> -q "SQL"` subcommand that uses
    `db.Exec` and prints `<n> rows affected`.
  - `SmoothDolt::exec` (Rust, CLI path) routes writes to the new
    subcommand instead of `sql`.

  Verified: create-then-read across `th pearls create` → row appears
  in subsequent `SELECT` from a fresh subprocess.

- 94987f4: `th pearls push/pull` is a no-op on the global store

  Project pearl stores are designed to sync via Dolt remotes
  (per-project board for the team). The global store at
  `~/.smooth/dolt` holds personal-scope state (sessions, memories,
  private pearls) and isn't meant to sync — making `th pearls push`
  fail there with "no configured push destination" was just noise.

  Now `th pearls push/pull` from the global store prints a one-line
  informational message and exits 0 instead of erroring. Project
  stores still surface the error so a missing remote on a shared
  board is obvious.

  Detection: canonical-path comparison against `~/.smooth/dolt`.
  Error matching is heuristic (looks for "no configured push
  destination", "no upstream", "remote not found", etc.) so
  unrelated SQL/lock errors still propagate.

- c237f11: Add `smooth-dolt-launcher` — clean-slate exec wrapper for spawn isolation

  Tiny C binary (~5 KB, ~30 lines) that runs BEFORE Go starts:
  resets the inherited signal mask, closes every fd > 2, `setsid`s,
  then `execv`s the requested program. Used transparently when
  `SmoothDoltServer::spawn_handle_once` launches `smooth-dolt serve`
  from inside Big Smooth's Tokio runtime.

  Without the launcher the child Go runtime can wedge on first SQL
  query in pearl `th-1a61a7`-style failures: Tokio installs blocking
  signal masks (Go needs SIGURG for goroutine preemption) and
  contaminates fd inheritance (Go grabs leftover Tokio epoll fds at
  startup). Restored daemons via this path get clean process state.

  The launcher is opt-in via path discovery — falls back to the
  shell-laundered spawn if the binary isn't installed alongside
  `th` and `smooth-dolt`. CLI invocations of `th pearls *` and
  short-lived parents work without it; long-running daemons
  (BS) benefit from it.

  Build: `bash scripts/build-smooth-dolt-launcher.sh`

## 0.12.5

### Patch Changes

- 5cc9640: Direct git-clone via bash + runner stderr logging

  Two fixes for the "ask BS to clone a repo, watch the spinner for 5
  minutes, get a wall error" failure mode:

  - **System prompt**: one-shot allowlisted writes (`git clone`, `gh repo
clone`, `mkdir -p`, `curl -o`) are now explicit `bash` territory.
    Previously the prompt blanket said "writes → spawn a teammate"
    which sent a 2 s clone through a 30-90 s teammate boot path that
    could (and did) wedge.
  - `mkdir` added to the bash allowlist; bash timeout bumped 10 s →
    30 s so a small clone fits.
  - **Runner observability**: `dispatch_ws_task_direct` now logs
    `tracing::info!` on spawn (PID + binary + cwd + model) and on the
    first stdout line. Runner stderr is mirrored to `tracing::warn!`
    so a wedge that prints a panic / init error is visible in
    `service.log` instead of disappearing into a WebSocket TokenDelta
    no one is reading.

## 0.12.4

### Patch Changes

- 0f969d8: Big Smooth flies down to the question while thinking

  When the chat agent starts thinking, the BS face fades out of the
  header and a fresh face flies in below the most recent user message
  (with the thought bubbles attached underneath). When the response
  lands, the message face vanishes and the header face fades back in.

  The fly-in uses a slight overshoot (`cubic-bezier(0.34, 1.56, 0.64, 1)`)
  so he lands with a bit of personality instead of a flat slide.

## 0.12.3

### Patch Changes

- b6b1699: Single-writer queue in front of smooth-dolt serve

  Concurrent dolt callers (chat agent + orchestrator + healthcheck +
  session save) could race each other into the Dolt manifest lock,
  producing intermittent "database is read only" errors. With this
  change every op for a given data dir is serialized through the
  server's `serial_lock` mutex — at most one in-flight write at a
  time, with the underlying socket timeout (15 s) bounding any
  single op.

  Combined with the 30 s healthcheck respawn loop, the connect-time
  self-heal in `client()`, and the 5-minute chat-turn ceiling, this
  closes the last common Dolt-as-daemon failure mode.

  - New `SmoothDoltServer::with_client(|c| ...)` is the public entry
    point for serialized ops. `client()` is still exposed for the
    health-check path which deliberately bypasses the lock so it can
    race with in-flight work and detect a wedge.
  - `SmoothDolt::{sql, exec, commit, log, push, pull, gc, status}`
    in server mode now route through `with_client`.
  - New unit test `with_client_serializes_concurrent_callers` —
    spawns 8 racing threads, asserts the high-water "inside the
    closure" count never exceeds 1.

## 0.12.2

### Patch Changes

- 0813a89: Self-healing dolt mid-session — fixes multi-turn-after-sleep wedge

  Big Smooth would lock up after macOS overnight sleep: the long-running
  `smooth-dolt serve` socket goes silent (child still alive at 0% CPU,
  just unresponsive), and any subsequent dolt-touching request blocks
  forever. Multi-turn chats died on the second turn.

  Fix:

  - `SmoothDoltServer` is now respawn-capable. Internal state moved
    behind a `Mutex<ServerHandle>`; `client()` self-heals on connect
    failure (kills + spawns a fresh child, returns the new socket).
  - New `is_healthy()` (3 s ping) + `ensure_healthy()` (probe →
    respawn-if-sick → re-ping). Background tokio task in BS startup
    pings every server (project + global) every 30 s and respawns any
    that have wedged.
  - `SmoothDoltClient::connect` applies a 15 s read/write timeout so
    a wedged peer surfaces as an `io::Error` instead of blocking.
  - `SmoothDolt::{sql,exec,commit}` retries once on transport-looking
    errors (broken pipe, timeout, closed connection, ENOENT on the
    socket) via `ensure_healthy` between attempts. SQL-engine errors
    (locks, syntax) propagate unchanged.
  - Hard 5-minute ceiling on `chat_handler` and the session-bound chat
    path so a wedge that slips through still returns an actionable
    error instead of leaving the user watching the spinner forever.
  - New `PearlStore::dolt_server()` accessor so the host process can
    register the global store in the healthcheck loop alongside the
    per-project servers.

  Tests: `is_transport_err` round-trip (broken-pipe / timeout get
  flagged, SQL errors don't).

## 0.12.1

### Patch Changes

- fa477d1: Add `pearls_list` chat tool — fixes deadlock when asking pearl-count questions

  The chat agent's `bash` tool would gladly run `th pearls list` to answer
  "how many open pearls do I have", but `th` re-enters Big Smooth's own
  dolt store via a fresh CLI subprocess, which deadlocks against the
  long-running `smooth-dolt serve` companion. The chat hung indefinitely.

  Fix:

  - New `pearls_list(status?, limit?)` chat tool that calls
    `state.pearl_store.list(...)` directly through the existing
    serve-backed handle. Answers in milliseconds.
  - `bash` tool gains an explicit forbid-list (`th`, `smooth-dolt`,
    interactive editors) so the model can't accidentally re-trigger the
    deadlock. Surfaces a clear error pointing the agent at the native
    pearl tools.
  - `bash` timeout tightened from 25 s → 10 s. Slow commands belong in
    a teammate, not blocking the chat agent.
  - System prompt explicitly steers pearl questions to the native
    tools.

  Verified: "how many open pearls do I have right now?" went from an
  infinite hang to a 4.0 s round-trip.

## 0.12.0

### Minor Changes

- 4725f91: Big Smooth chat UI: three.js animated face + live thought stream

  - Add a mesh-based face for Big Smooth in the chat header that uses the
    th-in-Smooth logo gradient (teal → blue), bobs and rotates calmly when
    idle, and switches to a faster scan + brighter glow when streaming.
  - Stream live "thoughts" via the Fast slot (Gemini Flash Lite) — every
    tool call and intermediate assistant turn is summarized into one
    short, first-person sentence and broadcast over the chat WebSocket
    as `BigSmoothThought`. The chat page surfaces the most recent three
    as floating bubbles next to the face, with the static "Big Smooth is
    thinking…" line removed (the face + bubbles convey it).
  - Rate-limited (Semaphore-capped at 2 in-flight) and non-blocking —
    the agent loop never waits on the summarizer.

- 5274e96: Big Smooth chat: faster, self-healing, more visible

  **Speed**

  - Chat agent default model flipped from `smooth-reasoning-kimi` (slow)
    to `smooth-coding` (MiniMax — fast AND tool-call-capable). Cuts the
    end-to-end "do I have a github repo for X" round trip from 60–90 s
    (teammate spawn) to ~25 s (direct bash).
  - New `bash` tool on the chat agent with a tight read-only allowlist
    (`gh git kubectl jq curl ls cat head tail wc grep rg fd find echo`),
    so simple lookups don't need to spawn a teammate. System prompt
    re-written to bias toward `bash` for one-shot lookups.
  - `teammate_wait` poll cadence dropped from 5 s → 1.5 s so the chat
    agent picks up `[IDLE]` / `[CHAT:TEAMMATE]` within one round-trip
    of the teammate posting it.
  - Thought summarizer concurrency raised 2 → 4 so bubble bursts surface
    faster.

  **Self-healing**

  - `SmoothDoltServer::spawn` now retries once after killing zombie
    `smooth-dolt serve` processes for the same data dir, fixing the
    recurring "did not create socket within 15 s" startup failure.
    Timeout bumped 15 → 30 s.
  - Global pearl store (`~/.smooth/dolt`) now uses the long-running
    `smooth-dolt serve` companion instead of per-call CLI subprocesses,
    dodging the Dolt manifest-lock races that produced "database is
    read only" errors when the chat handler tried to save messages.
  - `run_cli` captures stderr inline so failures surface a real reason
    instead of "rerun the CLI for stderr".
  - `coding_workflow::snapshot_workspace` refuses to recurse when the
    workspace looks like `$HOME` (or contains classic HOME children
    like `Library`/`Desktop`/`Documents`, or has > 200 top-level
    entries). Closes the runaway-copy hang that wedged direct-mode
    teammates whose `working_dir` defaulted to BS's cwd.

  **Direct-mode UX**

  - Orchestrator background loop is skipped when
    `SMOOTH_WORKFLOW_DIRECT=1`. Stops it from independently spawning
    microsandbox VMs (via Bootstrap Bill) for ready pearls when the
    rest of the system is meant to be sandbox-free.

  **Big Smooth face**

  - Sunglasses (two slim lenses + bridge + top frame + lens flash),
    fedora-style hat (crown + brim + teal hat band), and a thicker
    smirk mouth. Mouth opens a hair while thinking; a "lens flash"
    glimmers across the shades every couple seconds for cool factor.
    Face also bigger — 96 px on desktop (was 72 px).

  **Thought-bubble UI**

  - Bubbles moved to their own row beneath the title for visibility,
    with a green-tinted container so the row is obvious even before
    the first thought lands. TTL bumped 7 s → 14 s; bubbles persist
    after the reply so the user can read what BS was thinking.
  - "Big Smooth is thinking · · ·" placeholder bubble shown while
    streaming with no thoughts yet, with animated dots.
  - New `[Stop]` button replaces `[Send]` while the chat agent is
    in flight, with an `AbortController` so the user can reclaim
    the input if a long-running call gets stuck.
  - Heartbeat thoughts: when no new tool-call event has fired for
    ≥ 8 s, the streamer emits a fresh "still working" summary every
    ~11 s so a long `teammate_wait` doesn't leave the bubble row
    silent.

## 0.11.0

### Minor Changes

- fecf00c: Big Smooth becomes a conversational team lead, and operators can talk back. Plan: `~/.claude/plans/sorted-orbiting-hummingbird.md`.

  - **Big Smooth chat is now agentic.** `POST /api/chat` runs an `Agent` loop with six tools: `pearls_search`, `pearls_show`, `pearls_create` (auto-titled via smooth-summarize), `teammate_spawn` (with `working_dir` + `role`), `teammate_message`, `teammate_read`. Default model is the reasoning slot (smooth-reasoning-kimi); `model` field on `ChatBody` overrides per-request. System prompt is goal-first, bias-toward-action.
  - **Pearl-comment mailbox.** Operators read steering / direct-chat / answers via a 1.5 s comment poll, injected into the agent loop as user-turns. New `AgentConfig.chat_rx`. Prefix routing: `[CHAT:USER]`, `[STEERING:GUIDANCE]`, `[ANSWER:USER|SMOOTH:q-id]`.
  - **Operator-side `ask_smooth` and `reply_to_chat` tools.** Blocking and fyi modes. Shared `QuestionRegistry` resolves blocking calls when the matching `[ANSWER:*:q-id]` lands.
  - **Teammate registry + REST.** `AppState.teammates: OperatorRegistry`. `GET /api/teammates`, `GET/POST /api/teammates/{name}/messages`, `POST /api/teammates/{name}/shutdown`. Per-pearl `comment-tap` broadcasts `TeammateChat` / `TeammateSpawned` / `TeammateIdle` events.
  - **Bench through Big Smooth.** `smooth-bench` now POSTs `/api/chat` and polls the pearl until `[IDLE]` or quiescence, instead of calling `run_headless_capture` directly. `SMOOTH_BENCH_LEGACY_DIRECT=1` falls back.
  - **Env plumbing.** `SMOOTH_PEARL_ID` reaches every operator. `SMOOTH_WORKFLOW_MAX_ITERATIONS` and `SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS` flow through both dispatch paths and the inner agent loop.

  Web UI sidebar (Shift+ArrowDown cycle, Lead pinned + Teammates section) and SSE streaming + per-session chat budget are planned follow-ups (Phase 4 UI half + Phase 6).

## 0.10.0

### Minor Changes

- 8e3e7d6: `smooth-dolt`: add a long-running `serve` subcommand. Opens the embedded Dolt database once and accepts JSON-line requests over a Unix domain socket — eliminates the per-call subprocess spawn that was hanging Big Smooth's `/api/projects` handler on smoo-hub (see pearl th-1a61a7). Existing one-shot subcommands (`init`, `sql`, `commit`, `log`, `push`, `pull`, etc.) are unchanged so the CLI keeps working. Phase A of pearl th-1ff010 — a Rust client and Big Smooth integration land in subsequent commits.

### Patch Changes

- bbf42fc: `SmoothDoltServer::spawn` now launders the spawn through `/bin/sh -c 'exec setsid smooth-dolt serve ...'` with a cleared env, instead of `Command::new(smooth-dolt)` directly. The embedded Dolt engine inside `smooth-dolt serve` cannot run when its parent process is the long-running Big Smooth daemon (under launchd) — even with stdin/stdout/stderr all set to `/dev/null` it parks all goroutines in `pthread_cond_wait`. The intermediate shell + `setsid` detaches the new server into a fresh session, drops anything weird Big Smooth's tokio runtime had attached to the spawn, and the embedded Dolt comes up clean. Verified on smoo-hub: `/api/projects` now responds in <1s where it previously hung at 60s+.
- 1465c51: `SmoothDoltServer::spawn` now also sets stderr to `/dev/null`. Inheriting the parent's stderr (which under launchd points at `~/.smooth/service.err`, a regular file) wedges the embedded Dolt engine inside `smooth-dolt serve` — SQL queries park forever in `pthread_cond_wait`. The shell-spawned binary works fine because the shell connects stderr to a TTY or `/dev/null`. Verified on smoo-hub: same binary, same dolt dir, only difference is the inherited stderr fd.
- 7acd383: Bump default `max_tokens` from 8192 → 32768 across the operator stack. Reasoning-model coding slots (smooth-coding via MiniMax M2.7) burn 1k–4k tokens on chain-of-thought before any visible content; with 8192 there's not enough budget left for the actual response + tool-call JSON, so multi-arg edits truncate and the agent burns iterations recovering. Affected configs: `LlmConfig::openrouter`/`anthropic` defaults, `ProviderRegistry::resolve_slot`, and the in-VM `smooth-operator-runner` startup config.

## 0.9.4

### Patch Changes

- cb36d28: Pre-open every registered project's `PearlStore` at Big Smooth startup and reuse those handles in `/api/projects` and `/api/projects/pearls`. Calling `PearlStore::open` from inside a tokio handler reliably wedges the spawned `smooth-dolt` Go subprocess in `pthread_cond_wait` and never returns (observed on smoo-hub: 60s+ timeouts on `/api/projects` while the same operation from a TTY returned in 50ms; `state.pearl_store.stats()`, which uses a store opened at startup, worked fine in the same process). Pre-caching at startup avoids the bad code path entirely. Trade-off: project registry changes need a service restart to populate.

## 0.9.3

### Patch Changes

- 5e42e47: Fix smooth-dolt subprocesses hanging indefinitely when called from inside Big Smooth's tokio runtime. Root cause: smooth-dolt's Go runtime forks a long-lived `dolt sql-server` child that inherits the parent process's open file descriptors. When `SmoothDolt::run` connected stderr to a pipe (the default behaviour of `Command::output`), the daemon child held that pipe fd open after smooth-dolt itself exited; `Command::output` waited for EOF on the pipe forever. Observed on smoo-hub as `/api/projects` timing out at 60s+ while the same command from a TTY returned in 50ms. Fix is to redirect smooth-dolt stderr to `/dev/null` (`Stdio::null`) so there's no pipe to inherit; on non-zero exit we now surface "rerun the CLI for stderr" instead of the captured message.

## 0.9.2

### Patch Changes

- b38c035: Fix `/api/projects` and `/api/projects/pearls` hanging on Big Smooth when `smooth-dolt` is on slower storage. Both handlers were calling `PearlStore::open` + `store.stats()` / `store.list()` directly inside `async fn` bodies — those functions shell out to the `smooth-dolt` Go binary via blocking `std::process::Command::output`, pinning the tokio worker for the whole subprocess+IPC roundtrip. With multiple registered projects we did N×subprocess sequentially on a single worker, easily blowing past the request timeout (observed: 60s+ on smoo-hub, never returned). Wrapped both handlers in `tokio::task::spawn_blocking` so the work runs on the blocking thread pool and the runtime stays responsive.

## 0.9.1

### Patch Changes

- 783c264: Make `smooth-web` actually usable on phones. Chat now stacks vertically on mobile (single-pane: Chats list when no active chat, Conversation when one is selected, with a back button). The Send button collapses to icon-only under `sm:`. Pearls page now renders an inline project picker (cards, with open/in-progress/closed counts) instead of just printing "Select a project to view pearls" — the existing picker lived in the sidebar drawer which is hidden by default on mobile, so users couldn't find it. Layout `<main>` padding drops from `p-6` to `p-4` on mobile to reclaim ~16px on each side, and chat heights use `100dvh` instead of `100vh` so iOS browser chrome doesn't eat the input row. Inputs all set explicit `font-size: 16px` to prevent iOS Safari's tap-to-zoom behavior.

## 0.9.0

### Minor Changes

- c510661: Rip out the per-language test-output parsers (`parse_pytest`, `parse_cargo_test`). Scoring now runs the language's test command and hands the stdout to the `smooth-judge` routing slot with a strict JSON-only contract — works for pytest, cargo test, go test, jest, gradle, ctest, anything. `parse_judge_response` is unit-tested for code fences, prose-wrapped JSON, partial totals, and malformed output; the LLM call itself is `judge_test_output` and can be called directly by other callers.
- c50cf9e: **TEST phase + self-validating EXECUTE + loop v2.**

  New TEST phase runs AFTER the provided tests pass. Classifies the code (React component / API client / web flow / WebSocket / DB service / CLI / pure library / async code), picks the canonical test stack for that shape (MSW, Playwright, testcontainers, property-based via hypothesis/proptest/fast-check, …), installs missing deps, and writes boundary-pushing tests that exercise real behaviour — not another unit test, but MSW intercepting the actual `fetch` retry loop or a Playwright browser clicking through the actual flow. If its new tests reveal real bugs, the workflow loops back to EXECUTE with them as the next review findings; if they're all green the workflow moves on to FINALIZE. Routed through `smooth-reviewing` (adversarial test writing is closer to code review than fresh implementation). Skippable via `SMOOTH_WORKFLOW_SKIP_TEST=1` for benchmark runs where adding extra tests would change the score.

  EXECUTE prompt now demands the agent pick a **self-validation** check appropriate to the language (`cargo check`, `python -m py_compile`, `go vet`, `node --check`, `tsc --noEmit`, etc.) and run it before declaring done — no more handing off to VERIFY with code that won't compile. Agent-written tests are welcome but MUST land with their implementation in the same change (no orphan failing tests that reference unimplemented methods).

  Loop v2 stop conditions are budget + plateau, not a fixed iteration cap. `verify_signature` extracts pass/fail counts from each VERIFY and breaks early when the signature repeats (model going in circles). Budget short-circuit breaks when the next cycle would likely blow the cap. Default `max_outer_iterations` bumped 3 → 10 as a ceiling, not the governor.

  New thesaurus phrases for the TEST phase — "Writing tests…", "Mocking the network…", "Booting the browser…", "Red-teaming the code…", etc. Status-bar cycle includes them when TEST is active.

- e16232b: **CodingWorkflow** — first real per-phase dispatcher. ASSESS / PLAN / EXECUTE / VERIFY / REVIEW / FINALIZE each run their own `Agent` invocation through a different `Activity` slot: Thinking for ASSESS + FINALIZE, Planning for PLAN, Coding for EXECUTE + VERIFY, Reviewing for REVIEW. Previously Thinking / Planning / Coding / Reviewing were declared-only — no code path routed through them.

  ASSESS now emits a structured `## Goal Summary` section that's threaded through every later phase's user prompt so the agent stays anchored to the objective across review loops. REVIEW can refine the goal summary via an `## Updated Goal Summary` block when it realizes the understanding drifted. FINALIZE checks the final state against the Goal Summary, not just test results.

  Opt-in via `SMOOTH_WORKFLOW=1` in Big Smooth's environment. When set, Big Smooth serializes the `ProviderRegistry` via `ProviderRegistry::to_json` / `from_json` (new) and passes it to the sandboxed runner in `SMOOTH_ROUTING_JSON`. The runner deserializes and dispatches the workflow; otherwise falls back to the existing single-Agent loop.

  `AgentEvent::PhaseStart { phase, alias, upstream, iteration }` emitted at each node entry. TUI listens, tracks `current_phase` / `phrase_idx` in `AppState`, and renders the phase prefix + rotating thesaurus phrase in the status bar:

  ```
  ASSESS · smooth-thinking → kimi-k2-thinking | Pondering… | tokens: 1.2k | spend: $0.003
  ```

  `smooth_code::thesaurus` provides the rotating phrase lists (Pondering… / Hammering… / Nitpicking… per phase). Spinner ticks advance the cycle.

  Companion fixes: `SafehouseNarc` now routes through `Activity::Judge` instead of the Default slot (what the Judge alias was named for), and `ToolRegistry` is `Clone` so multiple phase Agents can share the same tool handles.

- c53943f: Add `th routing resolved` — hits the LiteLLM `/model/info` admin endpoint on each configured provider and prints the alias → concrete-upstream map. Answers "what model actually runs behind `smooth-coding` today?" without needing server-side access. Internally exposed as `smooth_operator::resolution::{fetch_model_info, parse_model_info, ResolvedModel}` so other callers (bench harness, TUI status bar) can reuse it.
- d54cc78: New internal crate `smooai-smooth-bench` — benchmark harness for Aider Polyglot single-task runs. Not part of the user-facing `th` binary; invoke via `cargo run -p smooai-smooth-bench --` or `scripts/bench.sh`. Dispatches to Big Smooth over the headless WebSocket path, runs the language's test command in the scratch work dir, and writes a scored `result.json` to `~/.smooth/bench-runs/<run-id>/`. Parsers for pytest and `cargo test` summaries included; Go / JS / Java / C++ command shapes wired but not exercised yet. SWE-bench, Terminal-Bench, batch mode, and the web scoreboard are separate pearls.
- 422d9a8: Make smooth-web a PWA. Adds `vite-plugin-pwa` with auto-update SW, generated `manifest.webmanifest`, and the new `th` icon as both favicon (16/32 multi-res ICO + PNG variants) and PWA icon set (192/512 + maskable). Adds iOS apple-touch-icon variants (180/167/152/120) and meta tags for Add-to-Home-Screen. The axum static handler now serves `.webmanifest` with the spec'd `application/manifest+json` MIME (mime_guess doesn't know about it).
- 4471d5f: - **Cost threading**: `AgentEvent::Completed` now carries `cost_usd`, and Big Smooth's sandboxed dispatch path forwards that into `ServerEvent::TaskComplete` instead of the hardcoded `0.0` it sent before. `LlmResponse.gateway_cost_usd` captures the authoritative gateway-reported cost (LiteLLM's `x-litellm-response-cost-*` headers, with `-margin-amount` / `-original` / the legacy `-response-cost` all checked); `CostTracker::record_with_cost` replaces local `ModelPricing` guesswork when the gateway reports a real number.
  - **Spend meter in the TUI**: status bar shows `spend: $X.XXX` next to the token count, accumulated from every `ServerEvent::TaskComplete` across the session. Renders `$0` on fresh sessions; three-decimal precision under $1, two-decimal above.
  - **Glob `@` autocomplete**: `@**/*.rs`, `@../**/(dashboard)`, `@~/dev/**/README.md`, `@apps/**/package.json` all resolve through `ignore::WalkBuilder` + `globset`, respecting `.gitignore`. Falls through to the existing path-prefix listing when the query has no glob metacharacters. `(parens)` from Next.js route groups match literally.
- e0f892c: The Line is now visible in two new places:

  - **README badge** — points at `docs/bench-badge.json` (Shields.io endpoint format), auto-updated on every release tag alongside `docs/bench-latest.json`. Thresholds: ≥80% brightgreen, ≥60% yellow, else orange. A partial-sample (budget-cap hit) shows a ⚠ suffix.
  - **`th bench score`** — new subcommand prints The Line baked into this binary at build time. Reads `docs/bench-latest.json` via a `build.rs` rustc-env injection and formats with the same human table `smooth-bench score` uses (shared via `Score::render_table()`). When no Line is baked in yet it prints a hint explaining how to produce one locally.

  Supporting changes: `scripts/the-line/render-badge.sh` (jq-based Shields endpoint renderer), wired into `.github/workflows/the-line.yml` + its dry-run harness. `Score::render_table()` in `smooth-bench` is now public so both the harness binary and the CLI can render identical tables.

- 5f6057c: TUI: `@` now expands paths (`@~/`, `@./`, `@../`, `@/`), mixes pearls into file search results, and `/` triggers anywhere in the input to discover slash commands — not only at input start.
- 59ee646: TUI: redesign `/model` as an activity-slot picker. Top level lists the 8 routing slots (Thinking / Coding / Planning / Reviewing / Judge / Summarize / Default / Fast) with their current model. Enter on a slot opens a sub-picker of candidate models; selecting one applies the routing and persists it to `~/.smooth/providers.json`. Up/Down navigates, Esc backs out (Models → Slots → closed) — previously the picker had no input handling at all and Esc didn't dismiss it.
- 15ef8c5: TUI: add `/rename <title>` to rename the current session from inside the chat, and load pearls in the background so the UI paints immediately instead of waiting for the `smooth-dolt` subprocess to list pearls at startup.

### Patch Changes

- 02aa0c3: Bench: enable-skipped-tests step. Aider Polyglot tasks intentionally ship with most of their tests disabled so the stub code compiles — Rust bowling has 30 of 31 marked `#[ignore]`, JS bowling has 29 `xtest`/`it.skip`/`test.skip`/`xit`/`xdescribe`/`describe.skip` variants. Without flipping these on, the harness scored a "solved" verdict off a single trivial case. Rust now runs `cargo test -- --include-ignored`; JS spec files get their skip markers rewritten (`xtest(` → `test(`, etc.) in the scratch dir before tests run. Source dataset is untouched; only the per-run copy is edited.
- f102738: JS bench command now runs `npm install` before `npm test` — tasks ship only a `package.json` with devDependencies (jest/babel), no `node_modules`. Java bench uses the bundled `./gradlew --no-daemon` wrapper so version drift between the task and the sandbox doesn't matter.
- 76bb2a1: Java skip-strip. Polyglot Java tasks ship with `@Disabled` / `@Ignore` on 30-of-31 tests (same pattern as Rust `#[ignore]` and JS `xtest`/`test.skip`). Without the strip, a Java bowling run scored 1/32. Harness now rewrites `@Disabled` / `@Disabled("reason")` / `@Ignore` / `@Ignore("reason")` annotations out of test files in the scratch work dir (only test files, not production code — avoids clobbering unrelated annotations like `@DisabledInNativeImage`).
- 503a590: Bench: strip agent-added test files before scoring. Polyglot scorer runs the test command over the whole work dir, so any `test_*.py` / `*_test.go` / `*.spec.ts` / `*Test.java` / etc. the agent added during EXECUTE would get counted and tilt the score. The harness now snapshots the original file set before dispatching to the agent, and after the run deletes any files that (a) weren't in the snapshot AND (b) match per-language test-file conventions. Non-test files the agent added (new helpers, modules) are left alone. Original test files are always preserved. Benchmark invariant: only the provided tests count.
- c50cf9e: CodingWorkflow loop v2: stop conditions are budget + plateau, not a fixed iteration cap. Default `max_outer_iterations` bumped 3 → 10; the real governor is `verify_signature`, which extracts pass/fail counts from each VERIFY and breaks early when the signature repeats (model going in circles). Budget short-circuit added too — if next iteration would likely blow the cap, break. `verify_signature` is unit-tested across pytest/cargo/go/jest summaries, compile-error lines, and progress deltas.
- 4a2ff1a: Bench: judge prompt and test commands tightened for suite-level summaries. Drop `cargo test --quiet` and add `-v` to `go test` so each runner emits per-case lines. Judge system prompt now has explicit scoring rules — `ok <package>` with no per-case detail maps to passed=1/total=1 instead of returning all zeros, which previously marked a passing Go suite as UNSOLVED. Build errors count as failed=1.
- 34d9d7a: Repo-aware EXECUTE + TEST phases. Prompts now instruct the agent to inspect the repo first — `package.json` scripts / `Cargo.toml` / `pyproject.toml` / `go.mod` / `Makefile` / `.github/workflows/` — and pick validation + testing tools that match what the project already uses. Generic defaults (`cargo check`, `py_compile`, `go vet`, MSW, Playwright) are fallbacks only; the TEST phase won't suggest Playwright for a pure CLI or MSW for a Rust crate. README overhauled with the new 7-phase workflow diagram (ELK renderer, orthogonal 90° lines) and the per-phase routing table.
- 6f5fc06: Tool-call wire format is now strictly canonical: `function.arguments` always serializes to a JSON-object string, never `"null"` or a primitive. Strict providers (qwen3-coder-plus on DashScope) reject anything else with `InternalError.Algo.InvalidParameter: The "function.arguments" parameter of the code model must be in JSON format.` Fix lives in `canonical_tool_arguments_json`; also replaces the streaming-parse fallback from `Value::Null` to `Value::Object(empty)` so malformed deltas don't poison the next-turn echo. New `smooth_operator::quirks` module is the home for future per-upstream-model tweaks — seeded with qwen3 / qwen-coder flags, otherwise empty.

## 0.8.0

### Minor Changes

- 02fd111: TUI autocomplete: `@` for file paths, `/` for slash commands.

  The file-reference autocomplete state has always existed in
  `smooth-code` but was never wired into the event loop or
  rendered. Now it is, plus a parallel slash-command flow.

  - **`@`** anywhere in the input box pops the file picker with every
    entry in the workspace file tree. Type to narrow by
    case-insensitive substring on filename.
  - **`/`** at the start of the input pops the slash-command picker
    listing every registered command (`/help`, `/clear`, `/model`,
    `/save`, `/sessions`, `/quit`, `/status`, `/compact`, `/diff`,
    `/tree`, `/fork`, `/goto`, …) with one-line descriptions. Type to
    narrow by case-insensitive prefix.
  - **Up/Down** arrows move the selection, **Tab** or **Enter**
    accepts, **Esc** closes the popup, typing a space ends the active
    query. Backspace past the trigger char closes it.
  - Popup is a floating overlay anchored just above the input box, so
    the eye doesn't jump far from where you're typing. Orange border +
    "▶ " marker on the selected row to match the rest of the brand.
  - New types: `CompletionKind { File, Command }`, detail line on
    `AutocompleteResult`, `trigger_pos` on `AutocompleteState`.
  - New methods: `activate_commands`, `update_command_query`,
    plus two regression tests.

## 0.7.1

### Patch Changes

- a4a5063: TUI colors: orange is the primary frame accent, green is a
  secondary accent on assistant labels and the banner gradient.
  Previously panel borders + the chat title used green, which made
  the input-box border blend with assistant labels — users reported
  they couldn't see where to type.

  - `panel_border(true)` → Smoo AI orange (`#f49f0a`), was green.
  - `title()` → orange, was green.
  - New `input_border(mode)` helper: the message-input panel gets an
    orange bold border in input mode and a gray border only when the
    user explicitly escapes into normal mode. The chat panel follows
    focus; the input panel stays obvious as "the place to type."
  - New "▶ Message" title on the input panel, orange + bold.
  - Assistant labels stay green (secondary accent), user labels stay
    orange (primary accent), banner keeps the orange→green vertical
    gradient — green lives on as the destination color.

  All colors verified against
  `smooai/packages/ui/globals.css` (the canonical palette): orange
  `#f49f0a`, green `#00a6a6`, red `#ff6b6c`, blue `#bbdef0`,
  gray-700 `#4e4e4e` all match.

  Regression test: `test_input_border_is_orange_in_input_mode_gray_in_normal`.

- f8f3ed3: TUI: remove CSI 2026 synchronized-output wrapper around each
  render. Fixes the "I can type but the screen doesn't update until I
  ^C" class of bug reported on at least one macOS terminal.

  Root cause: the event loop wrapped each `terminal.draw` with
  `print!("{}", begin_sync())` / `print!("{}", end_sync())`. On
  terminals that half-support CSI 2026 (or where `print!` doesn't
  flush between the begin and the end), frames get stuck in the
  terminal's buffer until the process exits and stdout flushes on
  teardown — so typed input appears to be ignored until you kill
  `th`.

  ratatui's backend already produces flicker-free output via
  crossterm's diff-based rendering, so the sync wrapper was a
  micro-optimization not worth the fragility it introduced. Dropped.

## 0.7.0

### Minor Changes

- 18e1398: `th code --resume` and auto-generated session titles.

  - **Session titles.** `Session` now carries an optional
    `title: Option<String>`. The TUI's input handler detects the first
    user message, spawns a detached `smooth-fast` call to generate a
    3–6 word Title Case summary, and stores it on `AppState`. Chat
    latency isn't gated on the name. Previously saved sessions without
    titles still load — `SessionSummary::display_label()` falls back
    to the message preview.
  - **`th code --resume [query]`**. New CLI flag. Resolution tiers:
    exact id → unique id prefix → unique title substring
    (case-insensitive) → unique preview substring. No argument picks
    the most recently updated session. Ambiguous matches error with
    the candidate list. Reuses the same auto-naming pipeline as the
    web chat so titles are consistent across TUI + web.
  - **`th code --list`**. Prints saved sessions newest first with
    display label, short id, and updated time, then exits without
    launching the TUI.
  - `AppState::from_resumed_session()` + `app::run_with_session()`
    restore a persisted session as the starting state. The welcome
    message is suppressed on resume in favor of a "Resumed session: &lt;title&gt;"
    marker.
  - Six regression tests on `SessionManager::find_by_query` +
    `most_recent` covering each tier and the ambiguous-match path.

## 0.6.0

### Minor Changes

- cf42e73: New `Activity::Fast` routing slot + LLM-generated session titles.

  **`smooth-fast` slot**: a new utility routing slot for short,
  latency-sensitive calls — session naming, short titles, autocomplete,
  one-liner tool-result summaries. Targets a Haiku-class model. The
  llm.smoo.ai gateway exposes it as `smooth-fast` (anthropic Haiku 4.5
  behind the alias).

  - `Activity::Fast` variant added to the routing enum.
  - `ModelRouting.fast: Option<ModelSlot>` — optional on disk, so
    existing `providers.json` files still parse. When absent,
    `slot_for(Activity::Fast)` falls back to the `default` slot.
  - Every preset (`SmoaiGateway`, `OpenRouterLowCost`, `LlmGatewayLowCost`,
    `OpenAI`, `Anthropic`) now configures a sensible `fast` slot
    (Haiku / gpt-4o-mini / gemini-flash-lite).
  - `th routing show` lists the new slot so users can see where
    utility calls go.

  **Session auto-naming**: first-message titles now come from the
  `smooth-fast` slot — 3–6 words, Title Case, trimmed — instead of a
  60-char truncation of the user's prompt. The LLM call is spawned
  into a detached tokio task so chat response latency is unaffected.
  On LLM failure we fall back to the legacy truncation so a session
  is never stuck at "New chat".

  Wire it up in your `~/.smooth/providers.json` once llm.smoo.ai's
  `smooth-fast` alias is live in prod:

  ```json
  "routing": {
    …existing slots…,
    "fast": { "provider": "smooth", "model": "smooth-fast" }
  }
  ```

## 0.5.5

### Patch Changes

- 6f9b259: TUI: drop "Coding", use the brand gradient for the wordmark.

  - "Smooth Coding" → "Smooth" everywhere user-visible (chat panel
    title, welcome message, doc strings). The product's name is
    "Smooth" — this is the coding surface of it, not a separate
    product.
  - New `theme::smooth_wordmark()` returns a `Vec<Span<'static>>`
    rendering "Smooth" with the same per-character gradient the CLI
    uses in `gradient::smooth()`:
    - `Smoo` → #f49f0a orange → #ff6b6c pink (linear over 4 chars)
    - `th` → #00a6a6 teal → #1238dd blue (linear over 2 chars)
      The chat panel border title now uses it, so the wordmark in the
      TUI matches the `th` CLI banner and the horizontal logo.

## 0.5.4

### Patch Changes

- 4abae68: th-324b12: instrument `smooth-code`'s TUI startup so "my terminal
  shows nothing" is diagnosable without a screen recording.

  Three additions to `crates/smooth-code/src/app.rs::run`:

  1. **TTY pre-flight.** If `stdin` or `stdout` isn't a terminal, fail
     with a clear message pointing at `th code --headless`. Previously
     the app would enter alt-screen, render to /dev/null, and exit
     cleanly — the user saw nothing and had no clue why. Also reliably
     caught via a regression test (`run_requires_tty`).

  2. **`SMOOTH_TUI_NO_ALT_SCREEN=1` escape hatch.** Some terminals
     (a few tmux configs, certain Windows terminals, odd ssh
     multiplexes) don't cleanly combine alt-screen + mouse-capture +
     CSI 2026 synchronized output. The env var drops alt-screen and
     mouse-capture so the UI renders inline in the primary buffer —
     scrollback gets mixed with the TUI output but at least the user
     can _see_ something.

  3. **`SMOOTH_TUI_DEBUG=1` step log.** When set, every major startup
     step (TTY check, raw-mode enable, alt-screen enter, Terminal
     creation + size, first draw, event-loop entry + exit, terminal
     restore) logs to `~/.smooth/logs/smooth-code.log` with a
     timestamp. Zero-cost when unset. Lets us trace exactly where
     `run()` gave up on an environment-specific blackout without
     needing a tmux capture.

  Also: initial forced `terminal.draw` before the event loop starts,
  so even if `event::poll` blocks for a long time on the first
  iteration, the welcome message is visible immediately. Previously
  the draw only happened at the top of the loop body, gated by the
  auto-save check — a startup stall could delay the first frame.

  Improved error messages on `enable_raw_mode` + `EnterAlternateScreen`
  failures suggest `SMOOTH_TUI_NO_ALT_SCREEN=1` as the first thing to
  try when a terminal silently rejects the setup.

## 0.5.3

### Patch Changes

- a85ea2b: Fix "Dolt store" showing red on the dashboard while green on
  `th status`. Pre-existing pearl stores were created before the
  `config` table was part of the schema (added in the retire-sqlite
  change), and only `PearlStore::init` ran `ensure_schema`. `open()`
  skipped it entirely, so `get_config("__health_check")` in the health
  handler ran `SELECT v FROM config WHERE k = ...` against a missing
  table, failed, and flipped `database.status` to `"down"`.

  `PearlStore::open` now runs an idempotent schema-migration check: a
  single `SHOW TABLES` query against the open store; if any required
  later-added table is missing, it re-runs the full `CREATE IF NOT
EXISTS` pass and commits. On an up-to-date store it's a single
  round-trip. Concurrent migrators are safe — duplicate commits are
  logged and swallowed.

  Added a regression test
  (`test_open_migrates_missing_config_table`) that simulates a legacy
  store by dropping `config`, reopens via `open()`, and verifies
  `get_config` / `set_config` work without error.

- 82cca37: Remove the `images` job from the release workflow until we fix
  smooth-dolt's aarch64-linux-musl cross-compile.

  Current state:

  - `ghcr.io/smooai/smooth-operator:0.2.0` / `:latest` and
    `ghcr.io/smooai/safehouse:0.2.0` / `:latest` are already public on
    GHCR (pushed manually the day we went public). Smooth pulls `:latest`
    by default so end users are unaffected.
  - The `images` job was green through docker login after the `GH_PAT`
    scope fix, but then failed on `build-safehouse.sh` — that script
    expects a cross-compiled `smooth-dolt` at
    `target/aarch64-unknown-linux-musl/release/smooth-dolt`, which
    nothing currently produces. `build-smooth-dolt.sh` is a host-arch
    `go build` that lands at `target/release/` (glibc-linked), so the
    alpine-based safehouse image can't copy it.

  Options for the follow-up (tracked in a pearl):

  1. Switch the safehouse image base from alpine to
     `debian:slim-aarch64` so a host-linked smooth-dolt runs natively.
  2. Cross-compile smooth-dolt to aarch64-musl using `zig cc` as the Go
     CGO compiler (the same zigbuild workflow Rust uses).
  3. Build smooth-dolt inside a containerized alpine stage during
     `docker build` and COPY the result.

  Until then, image pushes are manual via
  `scripts/build-smooth-operator-image.sh --push` and
  `scripts/build-safehouse-image.sh --push`.

- 83ba4d1: Fix **th-dfd0d3**: every sandboxed tool call was being rejected with
  "error decoding response body" because `WonkHook` inside the operator
  runner never carried the per-VM bearer token. The same security
  hardening commit (`f7676d8`) that added `Authorization: Bearer` auth
  to Wonk's `/check/*` endpoints updated `WonkClient` (used by Goalie)
  but left `WonkHook` (used by the agent's tool registry) untouched.
  Every `pre_call` → `/check/tool` now gets a 401 with an empty body,
  and `resp.json::<CheckResponse>()` surfaces as the opaque
  "error decoding response body" at the hook layer.

  Changes:

  - `WonkHook::with_auth(url, token)` constructor; `new` remains as
    a zero-token shim for legacy tests.
  - Per-request `Authorization: Bearer <token>` when the token is
    non-empty.
  - `check()` now inspects HTTP status before attempting to decode as
    JSON — on a non-success response we surface
    `"Wonk /check/... returned 401: <body>"` instead of the misleading
    decode error. Future misconfigurations will be obvious.
  - `smooth-operator-runner` stores the operator token on `Cast` and
    wires `WonkHook::with_auth(&cast.wonk_url, &cast.operator_token)`
    into the tool registry.
  - Regression tests on `WonkHook` pre-call:
    `pre_call_without_token_surfaces_401_not_decode_error` (negative)
    and `pre_call_with_auth_passes_through` (positive).

  Also fixed a CI-flaky test on the side: the two
  `smooai_gateway_*` provider tests both mutate the global
  `SMOOAI_GATEWAY_URL` env var and ran in parallel, racing each other.
  Added a module-local `Mutex` so they serialize.

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
      `libicu-dev` + `setup-go` for `smooth-dolt`, which the safehouse
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

  - **GHCR**: `smooai/smooth-operator` and `smooai/safehouse` images are
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

  Fix: publish `smooai/smooth-operator` and `smooai/safehouse` images
  to GitHub Container Registry (public), and default to pulling from
  there. The `Dockerfile.smooth-operator` pre-creates `/workspace`,
  `/opt/smooth/bin`, and `/opt/smooth/cache/mise` — so every bind-mount
  target now exists before the guest agent tries to mount on top of it.

  - `SandboxConfig` default image: `alpine` → `ghcr.io/smooai/smooth-operator:latest`
  - `th run` default: `smooai/smooth-operator:latest` → `ghcr.io/smooai/smooth-operator:latest`
  - `scripts/build-smooth-operator-image.sh` + `build-safehouse-image.sh`:
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
  - `docker/Dockerfile.safehouse` + `scripts/build-safehouse-image.sh`
    → `smooai/safehouse:<version>` (alpine + safehouse bin + smooth-dolt).
  - Both scripts delegate to the existing cross-compile flow
    (`build-operator-runner.sh` / `build-safehouse.sh`).
  - Fixed a latent package-name bug in `build-safehouse.sh`
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
