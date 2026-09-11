---
'@smooai/smooth': patch
---

th attest: clean the remote worktree after checkout so a stale box can't post a false red

`th attest` delegates `rust` to a build box (smoo-hub) by fetching the commit and
running `git checkout --detach --force <sha>`. That resets tracked files but leaves
untracked ones in place, so a file abandoned by an earlier branch (the incident was
a stray `api-prime/tests/*.rs`) survived into the tree under test — cargo compiled it
and the box posted a FALSE `ci-attest/rust: failure` naming a plausible cause, which
reads to a reviewer as a genuine Rust failure (th-5123e5).

The remote script now runs `git clean -ffd` after the checkout, so the worktree
matches the SHA exactly. No `-x`: the cargo cache lives in an external
CARGO_TARGET_DIR, so ignored files inside the worktree are cheap to keep and are not
what poisons the build. A clean failure exits as a precondition (97) like the cd /
fetch / checkout lines — a wrong box, never a verdict on the commit.
