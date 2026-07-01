---
'@smooai/smooth': patch
---

Fix `th api agents mint`: the assembled `CreateAgentRequest` was missing three NOT-NULL columns the create route requires — `organizationId`, `summary`, and `isBuiltin` — so every mint failed with a 400 (`expected string, received undefined` at `summary`/`organizationId`, `expected boolean` at `isBuiltin`). `build_mint_body` now sets all three (`isBuiltin: false`; `summary` from the new optional `--summary` flag, defaulting to the agent name). The stale doc comment claiming the backend generates the summary is corrected — the route only fills in the auth-public-client credentials and `createdBy`.

Note for cross-org minting: `th api` sends the org-locked M2M token, which can read a child org but not write to it. To mint into a child org as a parent-org admin, point the client at your user session (which acts cross-org): `SMOOAI_AUTH_FILE=~/.config/smooth/auth/profiles/<profile>/smooai-user.json th api agents mint …`.
