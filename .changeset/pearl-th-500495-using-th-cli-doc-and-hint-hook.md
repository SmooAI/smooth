---
"smooth": patch
---

docs: add `docs/Engineering/Using-th-CLI.md` covering the full `th api` / `th admin` (planned) / `auth.smoo.ai` OAuth2 client_credentials flow, plus a `.claude/hooks/th-curl-hint.sh` PreToolUse hook that nudges Bash commands toward `th` whenever they're about to raw-curl `api.smoo.ai`, `auth.smoo.ai/token`, or `atlassian.net/rest/api`. Hook also covers the `gh secret set --body -` newline footgun (SMOODEV-879) and raw `pnpm sst secret list` leakage (SMOODEV-908). Mirrored in the smooai monorepo so the same hints fire in both repos. Pearl th-500495.
