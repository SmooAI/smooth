---
"@smooai/smooth": patch
---

`smooth-agent` plugin accuracy pass (0.3.0 → 0.3.1) — every `th` invocation the
plugin ships is now verified against the current CLI, and one dead guardrail is
revived.

**Broken hook fixed.** `enforce-pearls-labels.sh` read a `$TOOL_INPUT`
environment variable that Claude Code never sets — hook input arrives as JSON on
stdin — so the label reminder had been a silent no-op since it was written. It
now parses stdin, matches the real flag (`--label`, singular), and points at the
command that actually exists (`th pearls label <id> add <label>`; `th pearls
update` has no label flag). Fails open when `jq` or the payload is missing.

**Stale `th` commands corrected in `th-curl-hint.sh`:**

- `th api login` → `th auth login --m2m` (the `th api` auth verbs are deprecated)
- `th api config …` → `th config …` — config is a top-level command, never lived
  under `th api`
- `th jira sync --pull` → `th jira sync` — the flag does not exist
- `th admin …` is no longer "planned"; it exists behind `--features admin`
- `--org` → `--org-id`, `th api help` → `th api --help`

**Docs/skills resynced:**

- README + marketplace no longer advertise "rate-limit-resilient" — the 429
  auto-retry was dropped in th-2d5c45; the supervisor now stops on a real
  usage/quota limit and lets Claude Code handle the transient throttle.
- README lists the `smooth-operator` skill (previously shipped but undocumented)
  and `th claude tui`.
- `/smooth` no longer runs `th msg inbox --pull` on every invocation — `--pull`
  writes to the shared Dolt store and repeated pulls wedge every agent's mailbox,
  which the plugin's own `agent-comms` skill warns about.
- `pearls-flow` drops the nonexistent `th pearls edit`, spells out the full 0–4
  priority scale, and documents the singular `--label`.
- `enforce-worktree.sh` allows `.smooth/` (the gitignored pearl store) alongside
  its dead `.beads/` predecessor. Its blocking logic is otherwise untouched.
