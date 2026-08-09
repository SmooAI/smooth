---
'@smooai/smooth': patch
---

Desktop 0.1.6: auto-update improvements — check every 30 min (was 6h, so beta builds arrived late), log all update activity to `~/.smooth/desktop.log` (the console was lost under a Finder/`open` launch, making "why didn't it update?" unanswerable), and disable the differential downloader (its blockmap never reassembled byte-exact after notarization, so it always failed the checksum and fell back to a full download anyway — now it full-downloads directly, no wasted bandwidth or scary error). The update path itself was already working end-to-end.
