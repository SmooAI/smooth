---
'@smooai/smooth': patch
---

Fix `th pearls remote add` mangling SCP-style SSH URLs. `git@github.com:SmooAI/smooth.git` was handed raw to Dolt, whose URL parser stored it as `git+ssh://git@github.com/./SmooAI/smooth.git` (bogus `/./`) — breaking push/pull. SCP-style URLs (`user@host:path`) are now normalized to the clean `git+ssh://user@host/path` form before Dolt sees them, in both `remote_add` and `clone_from` (so `th pearls init` bootstrap and recovery re-clone are covered too). All real URL forms (`https://`, `ssh://`, `git+ssh://`) and filesystem paths pass through unchanged. Pearl th-c4441b.
