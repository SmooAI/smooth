---
'@smooai/smooth': patch
---

Fix the release pipeline so the Homebrew tap update (and build/release) only run on the actual version-bump merge, not the version-PR run. `check_release` keyed `should_release` off `git log -1`, but the changesets action leaves HEAD on its own `🦋 New version release` commit on the `changeset-release/main` side-branch — so the gate matched in the version-PR run too, firing build/release/update-homebrew-tap against a half-merged tree and 404'ing the tap job on a not-yet-published asset (a spurious red failure every release). Now keyed off `github.event.head_commit.message` (the commit actually pushed to main), with an `origin/main`-tip fallback for `workflow_dispatch`. README: brew install stays the headline method, with a verify step. (pearl th-891ccb)
