---
'@smooai/smooth': minor
---

`th smoo sso` manages enterprise SSO for the active org: `connections list|show` reads the connections, `link preview|apply` shows which existing accounts in the verified domains are linked and links an explicit list (at most 200), and `require on|off` turns Require SSO on or off. `require on` prints how many unlinked accounts would be locked out and refuses unless `--acknowledge-lockout` matches that count. `th smoo roles default get|set|clear` reads or changes the role new members get. Every write confirms on a terminal or needs `--yes`; all of it needs `org.sso.manage` (plus the SSO product) or `org.roles.manage`.
