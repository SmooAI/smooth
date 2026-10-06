# Dev Loop and Landing

#engineering

> [!info] From CLAUDE.md
> Build/test/format commands, coding style, the oxfmt rules, testing requirements, changesets, and the full landing-the-plane checklist. Moved verbatim out of `CLAUDE.md` §2, §3, §8–§10.

## 2. Build, Test, Format, Lint

```bash
cargo build                  # Build all crates
cargo test                   # Run all tests (2000+ across the 12 crates)
cargo fmt                    # Format (rustfmt.toml: 160 width)
cargo clippy                 # Lint (pedantic + nursery)
cargo build --release -p smooth-cli  # Release binary (~10MB)
pnpm install:th              # Build web bundle + install th FROM LOCAL SOURCE (the dev test loop)
pnpm install:th:brew         # Install the latest RELEASED th via Homebrew (no source build; ignores local changes)
pnpm build:web               # Just rebuild the embedded web SPA
pnpm test:hooks              # Self-check the smooth-agent PreToolUse worktree guard
```

> **PreToolUse hooks block on exit 2 and ONLY exit 2.** Any other non-zero exit
> is a non-blocking hook error and Claude Code runs the tool anyway — which is
> how `enforce-worktree.sh` sat at `exit 1` and never blocked a single edit on
> main. `pnpm test:hooks` pins the exit codes; keep new deny paths at 2.

> **`pnpm install:th` installs to `~/.cargo/bin/th`, which does NOT automatically win on `PATH`.** The menu bar's "Install th CLI…" symlinks `/usr/local/bin/th` (or `~/.local/bin/th`) at `Big Smooth.app/Contents/Resources/bin/th`, and those dirs usually come first — so a successful dev install can silently keep serving the older bundled binary while you debug a stale `th` (pearl th-fd9d98 lost real time to exactly this). `install:th` now ends with `scripts/dev-link-th.sh`, which repoints that symlink at your build; it only ever rewrites a **symlink**, warns and leaves regular files (Homebrew, manual copies) alone, and is skipped by `SMOOTH_NO_DEV_LINK=1`. Check with `bash scripts/dev-link-th.test.sh`.
>
> **Sanity check after any install:** `th --version` prints the commit it was built from — compare it to `git log -1`. If they differ, you are testing the wrong binary.

### Web UI (crates/smooth-web/web/)

```bash
cd crates/smooth-web/web
pnpm install
pnpm build                   # Builds to dist/, embedded in binary
pnpm dev                     # Vite dev server at :3100
```

---

## 3. Coding Style

### Rust

- Edition 2021, max_width 160, field init shorthand
- `unsafe_code = "forbid"`, `unused_must_use = "deny"`
- clippy pedantic + nursery (warn)
- `anyhow` for errors, `thiserror` for library errors
- `tracing` for logging

### Web (TypeScript/React)

- Vite + React 19 + Tailwind CSS 4
- oxfmt for formatting, oxlint for linting

### Everything that is not Rust

**`oxfmt` is the only formatter in this repo** (pearl th-9bee92 removed the never-wired `dprint.json` and `.prettierignore`). `pnpm format` writes, `pnpm format:check` verifies, and the `format` job in `pr-checks.yml` runs it **ungated on every PR** — it is the one check a docs-only or changeset-only diff cannot skip. It owns `md`, `json`/`jsonc`, `yaml`, `toml`, `css`, and `js`/`ts`, so a markdown or changeset edit is **not** format-exempt. Exclusions (generated or vendored bytes) live in `.oxfmtrc.json` `ignorePatterns` — `CHANGELOG.md` is on that list because `changeset version` writes it, and oxfmt mangles the prose it appends.

> ⚠️ oxfmt formats markdown, and it reads bare `snake_case` in prose as an emphasis span — `transfer_call, notify_humans` comes back out as `transfer*call, notify_humans`. Backtick identifiers in docs; the corruption is oxfmt's own output, so `format:check` will _demand_ it once it lands.

## 8. Testing — MANDATORY

> **CRITICAL: Every crate, every module, every public function MUST have tests.** No code lands without passing tests. This is non-negotiable.

- Tests colocated in each module (`#[cfg(test)]`)
- `cargo test` runs all — **must pass before any commit**
- `cargo clippy` must be clean (zero warnings) before commit
- `cargo fmt -- --check` must pass before commit
- Test categories:
    - **Unit tests**: every public function, every error path, every edge case
    - **Integration tests**: cross-module interactions (e.g., policy → sandbox, sandbox → goalie egress)
    - **Property tests**: where applicable (e.g., policy round-trip serialization)
- When adding a new module: write tests FIRST or alongside, never "add tests later"
- When fixing a bug: add a regression test that fails without the fix
- Security-critical code (policy enforcement, access control, secret detection) requires **exhaustive** test coverage including adversarial inputs

---

## 9. Changesets & Versioning

Always add changesets when landing work — this is how versions get bumped and changelogs generated.

```bash
pnpm changeset        # Interactive changeset creation
```

- Config: `.changeset/config.json`
- `package.json` is the single source of truth for the version
- `scripts/sync-versions.mjs` propagates the version to `Cargo.toml` workspace.package.version and `Cargo.lock`
- Release automated via GitHub Actions (`release.yml`) — Changesets PR → auto-merge → multi-platform binary build → GitHub Release
- Changesets describe what changed and why for the changelog

---

## 10. Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

### Mandatory checklist

1. **Run quality gates** (if code changed):

    ```bash
    cargo fmt -- --check
    cargo clippy
    cargo test
    cargo build
    pnpm install:th    # Update ~/.cargo/bin/th to latest
    ```

2. **Add changeset** for version bump:

    ```bash
    pnpm changeset    # Describe what changed and why
    ```

3. **Close pearls** for completed work:

    ```bash
    th pearls close <id1> <id2> ...
    ```

4. **Merge to main** if on feature branch:

    ```bash
    cd ~/dev/smooai/smooth
    git checkout main && git pull --rebase
    git merge <branch> --no-ff
    ```

5. **Push to remote**:

    ```bash
    git push
    git status  # MUST show "up to date with origin"
    ```

6. **Clean up** — remove worktrees, delete merged branches

7. **Verify** — all changes committed AND pushed

### Critical rules

- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing — that leaves work stranded locally
- NEVER say "ready to push when you are" — YOU must push
- All tests, clippy, and format checks must pass
- If push fails, resolve and retry until it succeeds

---

## Related

- [[Home]]
