---
'@smooai/smooth': minor
---

pearls: compaction-proof handoff (th-9483e8) + SmoothFlow state hooks (th-1b8e05).
`th pearls checkpoint <id> [--note] [--next] [--auto]` records a checkpoint —
note + auto-collected worktree/branch/HEAD/dirty/session id — as a
`smooth-checkpoint:` JSON comment (public store API only — no schema work).
`th pearls show <id> --handoff [--json]` and
`th pearls prime --in-progress [--cwd .] [--assignee] [--json]` emit the
`{pearl, handoff, checkpoints, blocks, pr}` packet the SmoothFlow pearl rail
reads, or a compact "resume cold" block. `th pearls list --json` added. The smooth-agent plugin
gains `PreCompact` auto-checkpointing, a `SessionStart` (`compact|resume`) hook
that injects the handoff packets, and `flow-hook.sh` posting every lifecycle
event to the daemon's `/api/flow/hooks` (PermissionRequest decision passthrough,
everything else fire-and-forget, always exit 0 when the daemon is down).
