---
'@smooai/smooth': patch
---

CI: format-check every PR with oxfmt, the repo's single formatter

The repo carried config for two formatters — `.oxfmtrc.json` and `dprint.json` — and `pr-checks.yml` invoked neither. `package.json` had no `format` script at all, so markdown, JSON, YAML and changesets were unchecked; a malformed doc merged green. The `web` job compounds it by gating everything after its first step on `crates/smooth-web/web/**`, so a docs-only PR ran one grep and reported success in under ten seconds.

`oxfmt` wins on the merits: it is already what the two JS subprojects format with, the root `.oxfmtrc.json` is already its live config, and it covers `md`/`json`/`yaml`/`toml`/`css`/`js`/`ts` — a superset of `dprint.json`, which only ever declared the TOML plugin. `dprint.json` and the unused `.prettierignore` are removed; exclusions now live in `.oxfmtrc.json` `ignorePatterns` (generated and vendored bytes only: the obsidian vault internals, bench session dumps, the minified `openapi.json` snapshot, lockfiles, and the changesets-generated `CHANGELOG.md`).

The new `format` job is deliberately ungated — no path filter, no `if:` — because a check that skips itself on the diff in front of it is the bug being fixed here. `pnpm pre-commit-check` runs `format:check` too, so the failure surfaces before the push.
