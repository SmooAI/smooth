---
"@smooai/smooth": patch
---

GitHub release notes now lead with copy-pasteable install + upgrade commands instead of just the bare changelog. New `scripts/build-release-notes.sh` renders an Install section (Homebrew first, then `curl | sh`, then `cargo install`), an Upgrade section (one-liner per channel), the version's CHANGELOG.md extract, a Downloads table populated from the live release assets (with a fallback to the workflow's expected names when run before the release is created), and a footer linking the source / README / tap. Wired into `release.yml`'s `Create Release` job via `body_path`. v0.13.7 retroactively re-rendered with this format. Pearl th-release-notes.
