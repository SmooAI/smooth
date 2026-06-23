---
'smooai-smooth-daemon': patch
---

Phase 4 (EPIC th-c89c2a): auto-title sessions from their first message. On the
first `TaskStart`, an untitled session gets a readable title derived from the
first non-empty line of the user's message (trimmed to 60 chars, ellipsised) —
so the control surface's sessions list shows something meaningful instead of a
raw id slice. Adds `SessionStore::set_title_if_unset` (in-memory + SQLite
impls), which never clobbers a title the operator chose explicitly. Tested:
title-if-unset fill/keep semantics and the `derive_title` truncation/empty
cases.
