---
'@smooai/smooth': patch
---

Route all user-text-into-SQL sites through one correct MySQL/Dolt-dialect escaper. Dolt treats backslash as an escape character inside string literals, so the previous quotes-only escape was both a crash and an injection surface: input containing `\'` became `\''`, the backslash ate the first quote, and the remainder was parsed as SQL. The shared `sql_escape` now escapes backslashes before quotes and handles NUL, and every pearl/memory/message/agent/session SQL builder uses it.
