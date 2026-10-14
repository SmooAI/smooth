---
'@smooai/smooth': patch
---

`th harness enable claude-code` and `th doctor --setup-statusline` now move a statusline pinned to an old smooth-agent plugin version to the newest one, so the statusline picks up new segments (the context meter) after an update. A statusline that isn't ours, or a copy outside the plugin cache, is left alone.
