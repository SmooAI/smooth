---
'@smooai/smooth': patch
---

Big Smooth and `th` now always read the same Smoo AI credentials. Signing in through the daemon's browser flow left the `th` tool it shells out to logged out, because the daemon's credential path depended on **who launched it**: only `th` resolved the active auth profile (exporting `SMOOAI_USER_AUTH_FILE` / `SMOOAI_AUTH_FILE`), so a daemon started by `th up` inherited the right files and was accidentally correct, while one started by launchd or `nohup smooth-daemon` — how smoo-hub runs it — silently fell back to the legacy `~/.smooth/auth/*` tree. Profile resolution moved to `smooth_policy::auth_paths` (shared by both binaries) and `smooth-daemon` now resolves it at startup, so it no longer matters how the daemon was started.

Also removes the deprecated `th api login` / `logout` / `whoami`, superseded by `th auth login [--m2m]` / `th auth logout` / `th auth whoami`. Two spellings for one identity was actively confusing, and only `th auth` understands auth profiles. The `th api <resource>` verbs are unaffected. Docs corrected: `th auth login` is Smoo AI identity, not LLM-provider auth.
