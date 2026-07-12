---
"@smooai/smooth": patch
---

th CLI: plural⇄singular command aliases across the board. Every resource-noun command GROUP in `main.rs` (`Commands` + `ApiCommands`) and the `smooai/*.rs` subcommand enums now accepts either form via clap `visible_alias` — e.g. `th api agents`⇄`th api agent`, `th api keys`⇄`th api key`, `th orgs`⇄`th org`, `th operatives`⇄`th operative`, `th api crm contacts`⇄`th api crm contact`, `th testing runs`⇄`th testing run`. Verbs, positional args, gerunds, and acronyms are deliberately not aliased; top-level `th agent` (agent messaging) is intentionally NOT aliased to `agents` to avoid a semantic collision with `th api agents` (agent CRUD). Adds a `/normalize` skill (`.claude/skills/normalize/`) that audits every clap enum for resource-noun groups missing their counterpart alias and reports the gap table.
