---
'smooai-smooth-cli': minor
---

SMOODEV-2721: `th testing coverage report <lcov> --scope X` (parses LCOV in-CLI — totals + per-file, DA-fallback when LF/LH absent — and uploads to the new `/testing/coverage` endpoint; branch/commit default from GitHub Actions env) and `th testing coverage diff --branch X --base main --format table|md` (latest-per-scope vs baseline with per-scope deltas; md mode renders the GitHub PR-comment table). LCOV is the polyglot interchange, so one command covers vitest/cargo-llvm-cov/coverage.py/coverlet output.
