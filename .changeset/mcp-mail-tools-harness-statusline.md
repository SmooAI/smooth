---
'@smooai/smooth': minor
---

Agent mail becomes MCP tools every harness can reach, plus an optional cloud backend.

The machine-level mailbox existed but only the `th` CLI could reach it, and only
Claude Code sessions ever learned their own handle. Six new MCP tools on
`th mcp serve` — `agent_identity`, `agent_status`, `agent_list`, `mail_inbox`,
`mail_send`, `mail_ack` — put it in front of any harness, and their descriptions
carry the coordination conventions: typed mail, ack-after-handling, the handoff
body template, and that a `request` from another agent is information rather
than authorization. Identity is an explicit `agent_id` or `$SMOOTH_AGENT_HANDLE`;
with neither, the tools error rather than inventing a `user@host` identity and
writing to a mailbox nobody reads.

`th mcp install --harness claude-code|codex|opencode|all` registers that same
stdio server with each harness, so sessions in all three share one mailbox and
one pearl store. It is idempotent and preserving — JSON keeps its key order,
`~/.codex/config.toml` keeps its comments, keys you added yourself survive, and
a config it cannot parse is an error rather than being replaced. The
`smooth-agent` Claude Code plugin now ships the server in its manifest, so
plugin users get it for free.

A Claude Code statusline (`⚙ th:fix-auth ✉3`) keeps the handle and unread count
on screen instead of scrolling off the top of the transcript. `th doctor
--setup-statusline` wires it up and never overwrites an existing `statusLine`;
when one is present it offers a wrapper that renders ponytail's alongside ours.
New `th msg unread-count` is the bare number it reads.

`th agent backend set cloud` moves the mailbox to your Smoo user account so
agents on *different machines* share one bus — the one thing local SQLite cannot
do. It is entirely optional: SQLite stays the default and every command works
against it with no account, no network, and no configuration. Cloud needs
`th auth login` and is paid after a 14-day trial; there is deliberately no silent
fallback and no offline queue, and mail is not migrated between backends.
`th agent backend status` shows where you are and how the trial stands.

Also: the `attest-push-hint` hook now suggests `th attest` and fires on the
convention (an executable `scripts/ci/<name>.sh`) rather than one repo's copy of
the bash runner; Big Smooth's `th` tool description finally maps `msg`/`agent`,
so the daemon reaches for mail at all; and the `agent-comms` skill was rewritten
(it still documented the dead `<user>@<host>/<repo>` handle scheme and the Dolt
`--pull` footgun). See ADR-010.
