---
'@smooai/smooth': patch
---

Fix the legacy-auth migration stranding a valid session. `migrate_legacy()` skipped the migration whenever the XDG auth directory merely *existed*, but an earlier run with nothing to migrate creates that directory empty and returns — so any session written to the legacy `~/.smooth/auth` afterwards (what the daemon's browser login does) could never migrate. Both `th` and the daemon then resolved the empty default profile and reported "Not logged in" while a perfectly good session sat in the legacy tree. The guard is now content-based: migrate unless the XDG tree actually holds a session, in the default profile or any named one.
