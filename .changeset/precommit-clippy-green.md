---
'@smooai/smooth': patch
---

Re-green the pre-commit hook: `cargo clippy --workspace --all-targets` (what
`pnpm pre-commit-check` runs, stricter than CI's lib-only pass) was red on main
with four test-code errors, forcing every commit to `--no-verify`. Targeted
allows with comments: the ENV_LOCK guards in mail/mcp tests are held across
await on purpose (they serialize process-global env mutation), and the iMessage
fixture tuple is a documented test shape.
