---
"@smooai/smooth": patch
---

Big Smooth chat sessions now run as the operator's **real** Smoo AI org instead
of the `"local"` placeholder (pearl th-0c63cc).

The daemon installed the engine's `LocalTokenVerifier`, whose constructor
hardcodes `Principal::new("local", "local", …)`. Every `/ws` connection — and so
every conversation the daemon stored — carried `org_id = "local"`, which is not
an org any org-scoped tool can use: web search, knowledge lookup and scraping had
no tenant to act as and failed. Meanwhile the daemon has held a perfectly good
signed-in Smoo session next door the whole time (`th auth login`, kept fresh by
the credential heartbeat). The two auth systems simply never met — login was
working correctly; this was a handoff gap.

`smooth-daemon` now installs its own `SmooOrgVerifier`: the same local-token gate
(same length-aware constant-time compare, same fail-closed behavior on a wrong or
empty token), but the `Principal` it returns carries `active_org_id` and the user
identity from the stored user credentials. The credentials are read **on every
`verify()` call**, not cached at construction, so a heartbeat rotation or an
`orgs switch` takes effect on the next connection instead of needing a daemon
restart. Signed out — no credentials, an unreadable file, or no active org — it
falls back to `"local"` exactly as before, so the logged-out UX is unchanged.

**Note:** conversations created before this change are stamped
`organizationId = "local"` and the sidebar lists conversations by the connection's
org, so prior history will not appear in the list once a real org is in play. The
data is intact in the daemon's sqlite store and resuming a conversation by id
still works (its org comes from the conversation record, not the connection). No
re-stamping migration ships here.
