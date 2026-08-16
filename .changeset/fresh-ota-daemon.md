---
'@smooai/smooth': patch
---

Desktop OTA never ships a stale daemon again (th-76a353). The daemon binary now bakes its build commit into `--version` (`smooth-daemon 0.35.8 (sha)`) via a new build.rs with `rerun-if-changed=.git/HEAD` — the same mechanism `th` already had, which the daemon lacked, so a cached target dir had been serving an old daemon stamped with the new version (every OTA silently re-shipped old code). Plus a desktop-publish guard step that refuses to bundle if the built `smooth-daemon`/`th` commit != HEAD, so a stale build fails loudly in CI instead of shipping.
