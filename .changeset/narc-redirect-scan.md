---
'@smooai/smooth': patch
---

Narc's shell detector no longer flags `2>&1`, `>&2`, `/dev/` targets or a quoted `>` (such as `node -e "x => x"`) as a file truncation, so ordinary build and test commands skip the judge. It now checks every redirect in the line, not only the first, so `cmd 2>&1 > out.txt` is still caught.
