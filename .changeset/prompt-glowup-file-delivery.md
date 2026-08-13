---
'@smooai/smooth': patch
---

Prompting glow-up — Big Smooth reliably delivers files. The persona never told the model that MAKING a file isn't DELIVERING it, and never named `send_file`/`create_artifact`, so "make me an HTML file" ended at write-to-disk. Added a delivery rule + a roster of the unnamed tools to the persona; `create_artifact` now says it only writes locally and to call `send_file` to hand it over (both its description and its return message); `grep`/`write_file` got proper "use this when" guidance with the send_file delivery link.
