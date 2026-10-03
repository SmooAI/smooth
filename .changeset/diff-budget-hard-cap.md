---
'@smooai/smooth': patch
---

SmoothFlow diff: the 512 KiB frame budget is now a hard cap. Every file's summary stub is reserved up front, so a listing with hundreds of changed files no longer grows past the budget (and past what a phone can receive over the relay) (th-b994a1).
