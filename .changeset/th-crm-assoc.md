---
"@smooai/smooth": minor
---

`th api crm assoc` — CRM entity associations from the CLI (SMOODEV-2644).
Adds `assoc link`/`unlink`/`list`/`set-type`/`set-label`, where entities are
given as `TYPE:REF` operands (e.g. `contact:jane@x.com`, `company:Acme`,
`deal:"Big Deal"`, or `task:<uuid>`) — contact/company/deal refs resolve by
name/email/title via the existing resolvers, other types accept a uuid.
Also adds thin sugar wrappers over the legacy FKs: `contacts set-company`,
`deals set-contact`, `deals set-company`, and `companies set-parent` (each
accepting `none`/`-` to clear). Backed by the native api-prime associations
endpoints from SmooAI/smooai#3068; the commands 404 against prod until that
deploys.
