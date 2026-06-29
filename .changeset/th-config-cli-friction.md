---
'@smooai/smooth': patch
---

Fix several `th config` CLI friction points (found bootstrapping customer-org config):

- **push error text**: stop naming a nonexistent "@smooai/config build step" — now points to `th config init` (scaffold) or `th config pull` (fetch remote), and references the generator pearl (th-4d1d6c).
- **`--schema-name` create-vs-update**: clarified that `--schema-name` selects an existing schema to *update*; to *create* one, omit it and set `$smooaiName` in schema.json. The schema-not-found error now spells this out.
- **multi-schema `pull`**: a new pure `resolve_pull_schema` refuses to silently pick when an org has >1 remote schema — it lists the names and requires `--schema-name` (5 unit tests). One schema still auto-selects.
- **`th admin config`** help now points to the public `th config environments …` path (parent-org-admin-friendly; `th admin` is internal/org-locked).
- Replaced a stale `smooai-config` CLI reference with `th config push`.
- Documented the `.smooai-config/` layout + `schema.json` wire format in `docs/Engineering/Using-th-CLI.md`.
