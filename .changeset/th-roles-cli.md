---
'@smooai/smooth': minor
---

`th roles` — first-class CLI for editing an org's RBAC roles (SMOODEV-2368 /
ADR-105), so you can manage the custom-role catalog without clicking through the
web dashboard or hand-rolling curl against a user JWT.

Subcommands (all take `--org`/`SMOOAI_ORG_ID` and `--json`): `list` and `show`
(full permission-key list) read the catalog; `create <name>` makes a custom role
(with `--template <kind>` it seeds from an archetype via
`/workforce/role-templates/create-role`); `delete` removes a custom role;
`grant`/`revoke` add/remove permission keys with a read-modify-write that
preserves every existing key exactly (PATCH the whole array), `set-permissions`
replaces the set wholesale; `member-roles <email>` shows a member's roles and
`assign`/`unassign` edit them (read-modify-write over the replace-all PUT).

System roles (`organizationId == null`) are immutable, so
grant/revoke/set-permissions/delete refuse them locally with a clear error
before any API call. Keys that don't match `[a-z0-9_.*-]` warn but still send
(the server is the source of truth for validity). Rides the user JWT
(`th auth login`) because the roles routes 401 under an M2M token.
