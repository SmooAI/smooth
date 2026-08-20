---
'@smooai/smooth': patch
---

Three safety fixes: the worktree hook that never blocked, the operator token the sandbox let a hijacked shell read, and `th auth profile rm ..`.

`enforce-worktree.sh` exited 1 on both deny paths. Claude Code's PreToolUse treats only **exit 2** as blocking — every other non-zero exit is a non-blocking hook error and the tool call proceeds — so the hook that both repos' CLAUDE.md advertises as "will block source code edits and commits on main" has never blocked anything (18 transcripts fired it, zero acted on it; the sibling `attest-push-hint.sh` was moved to exit 2 and this one was missed). Both are exit 2 now. The Bash arm also only matched `git commit`, so `sed -i`, `cat > file`, `tee` and `rm` edited main with no hook firing at all; it now blocks a shell command whose mutation target is a file git tracks in the main worktree. `pnpm test:hooks` pins all of it, exit codes included.

The kernel sandbox denied the crown-jewel credential dirs but not the daemon's own runtime state. `~/.smooth/operator-token` is the bearer for `ws://127.0.0.1:8787/ws`, so a sandboxed shell — say, one that ate prompt injection from scraped content — could read it, open a *fresh* connection as the owner principal outside the conversation's permission mode, and write `~/.smooth/schedules.db` to get a second turn later. That is the same persistence primitive as the already-denied `~/Library/LaunchAgents`. All three (`operator-token`, `operator-storage.db`, `schedules.db`) are now read- and write-denied, matched by regex so the SQLite `-wal`/`-shm` siblings are covered too.

`th auth profile rm ..` deleted the entire auth directory. `valid_profile_name` permitted `.` and `..` (a dot is legal inside names like `tara.offsetwell`), and `auth_dir()/profiles/..` resolves to `auth_dir()` — so `remove_dir_all` took every profile, `smooai-user.json`, `smooai.json` and the `active` pointer. The existing test pinned `"../etc"`, which is rejected for the `/`, so the two names that actually traverse were never tried. Both are refused now, in the one validator every caller routes through.
