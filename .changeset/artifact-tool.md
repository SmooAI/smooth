---
'@smooai/smooth': patch
---

Add a `create_artifact` agent tool (smooth-tools): Big Smooth can now write a self-contained HTML report/artifact into `<workspace>/.smooth-artifacts/` and get back the absolute path plus a clickable `file://` URL — Claude Code Artifacts style. Registered in the default tool set so every host (including the daemon) gets it. Rendering artifacts inline in smooth-web is a follow-up.
