---
"@smooai/smooth": patch
---

Repo-aware EXECUTE + TEST phases. Prompts now instruct the agent to inspect the repo first — `package.json` scripts / `Cargo.toml` / `pyproject.toml` / `go.mod` / `Makefile` / `.github/workflows/` — and pick validation + testing tools that match what the project already uses. Generic defaults (`cargo check`, `py_compile`, `go vet`, MSW, Playwright) are fallbacks only; the TEST phase won't suggest Playwright for a pure CLI or MSW for a Rust crate. README overhauled with the new 7-phase workflow diagram (ELK renderer, orthogonal 90° lines) and the per-phase routing table.
