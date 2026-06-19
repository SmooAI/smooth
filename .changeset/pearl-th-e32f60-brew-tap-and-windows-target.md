---
"@smooai/smooth": minor
---

`brew install SmooAI/tools/th` — smooblue-parity install story (pearl th-e32f60). New `update-homebrew-tap` job in `release.yml` regenerates `Formula/th.rb` in [SmooAI/homebrew-tools](https://github.com/SmooAI/homebrew-tools) on every tagged release: fetches the three Unix asset tarballs, computes sha256, writes the formula with macOS arm64 + Linux x86_64 + Linux arm64 URLs, commits + pushes via SSH deploy key. Bootstrapped at v0.13.7 so the tap works today; subsequent releases will switch asset naming to `th-{macos-arm64,linux-x86_64,linux-arm64}.tar.gz` for parity with smooblue's convention. Windows target is filed as follow-up pearl th-a165b4 — needs workspace-wide Cargo feature gating (`default = ["desktop"]` / `cli-windows = []`) so the binary excludes microsandbox + ratatui on Windows.
