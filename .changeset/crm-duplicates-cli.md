---
'@smooai/smooth': minor
---

`th smoo crm duplicates` (alias `dupes`) reviews the CRM's AI duplicate-contact queue. `list` groups the open pairs by person and stars the suggested keeper. It prints a ready-to-run merge line only when the AI judged every pair in the group the same person. `merge-group --keep <id> <ids…>` folds a group into one record. `approve`, `reject` and `undo` act on a single pair, and `history` lists past decisions. Merges and undo confirm first (`--yes`, `--dry-run`). They need `crm.contacts.merge` and CRM Pro. It is the CLI twin of the copilot's and the hosted MCP's duplicate tools (SMOODEV-3859).
