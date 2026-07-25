---
'@smooai/smooth': patch
---

SMOODEV-2720: `th jira sync` is now safe by default — it only reconciles (closes pearls whose Jira tickets are all Done, transitions Jira tickets to Done once every referencing pearl is closed). The old unconditional mass-creation moved behind explicit `--pull` (Jira→pearls) and `--push` (pearls→Jira) flags, and `--dry-run` previews the full plan. Also fixes the sync reconciling against `PearlQuery::new()`'s default 100-row unfiltered slice (now loads all pearls) and key matching that treated placeholders like `SMOODEV-XXX` as issue keys.
