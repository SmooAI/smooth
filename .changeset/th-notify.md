---
'@smooai/smooth': patch
---

`th notify <message>…` — an agentic notify-the-human primitive. An AI agent running under `th` (Big Smooth / claude-driver) calls `th notify "blocked, need input"` to send a PUSH + in-app notification to the operator's OWN phone via `api.smoo.ai`.

The message is a positional joined with spaces, so `th notify done, review the PR` works unquoted (the way an agent would call it). Options: `--title` (default "Smoo AI"), `--priority low|medium|high|critical` (default medium), `--url <deepLink>`, and `--org-id`/`--org` to override the active org.

Authenticates as the logged-in user (`th auth login`), so there's no target to address — the human behind the session is the recipient. Wraps `POST /organizations/{org_id}/notifications/self`; prints `✓ Notified <you@email> — pushed to N device(s)`, with a hint to open the Smoo AI app when no devices are registered.
