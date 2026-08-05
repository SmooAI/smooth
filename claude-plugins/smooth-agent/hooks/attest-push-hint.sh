#!/bin/bash
# attest-push-hint: PreToolUse Bash hook that nudges the agent toward
# `scripts/ci/attest.sh` instead of a bare `git push`, in any repo that has one.
#
# Why a hook and not a git hook: `attest.sh` PUSHES (that ordering is what stops
# the credit losing the race against the runner), so a git `pre-push` hook calling
# it would re-enter itself forever. And the repo's own pre-commit chain
# deliberately "drops the full test suite (PR Checks owns that)" — attestation IS
# that suite, so it doesn't belong in a blocking git hook either. A Claude
# PreToolUse nudge has neither problem: it fires once, on the agent's own command,
# and the agent can decline with a reason.
#
# Exit codes: 0 allow silently, 1 ask the user (stderr hint is visible to Claude),
# 2 hard block. We use 1 — this is a nudge, not a gate. Attesting is often the
# WRONG call (a doc typo doesn't need 10 minutes of checks), so the agent has to
# be able to say no.
#
# Background: scripts/ci/README.md in the consuming repo · pearl th-6578ee

INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
[[ "$TOOL_NAME" != "Bash" ]] && exit 0

CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
[[ -z "$CMD" ]] && exit 0

# Bypass when the agent has already justified it.
echo "$CMD" | grep -q 'attest:ack' && exit 0

# Only care about an actual `git push` that publishes commits. Skip the forms that
# publish nothing a CI run would check: tag/branch deletion, --dry-run, and
# `th pearls push` (a Dolt sync, not git).
echo "$CMD" | grep -qE '(^|[;&|]|\s)git\s+(-[^ ]+\s+)*push(\s|$)' || exit 0
echo "$CMD" | grep -qE '(--delete|--dry-run|-d\s|--tags)' && exit 0

# Repo-agnostic by design: the hook fires only where the convention exists, so
# copying scripts/ci/ into another repo turns this on there with no extra wiring.
ROOT=$(cd "$(echo "$INPUT" | jq -r '.cwd // "."' 2>/dev/null)" 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null)
[[ -n "$ROOT" && -x "$ROOT/scripts/ci/attest.sh" ]] || exit 0

# What could be credited here, straight from the directory — no list to keep in sync.
CHECKS=$(cd "$ROOT/scripts/ci" 2>/dev/null && ls *.sh 2>/dev/null | sed 's/\.sh$//' | grep -vE '^_|^attest$' | tr '\n' ' ')

cat >&2 <<EOF
⚠️  attest-push-hint: bare \`git push\` — CI will redo work this machine can do.

Run \`bash scripts/ci/attest.sh <checks>\` INSTEAD of \`git push\`. It runs the
same scripts/ci/<check>.sh the workflow runs, THEN pushes, THEN posts a
ci-attest/<check> commit status. Each credited row skips in ~8s instead of
5-38 minutes.

  available: ${CHECKS:-none}

Pick what the diff actually touches — attesting everything is usually wrong:
  rust/**            → rust        (38 min on CI, ~5 min warm here — the big one)
  packages/, apps/   → typecheck lint test build
  docs/, comments    → nothing; just push

Two things to weigh before attesting:
  · attesting \`test\` also skips its coverage artifact + sticky PR comment.
  · a red local check may be the MACHINE, not the code. Check \`uptime\` first —
    timing-sensitive tests fail under load and pass quiet.

The order matters: attest.sh pushes for you. Pushing first loses the race, because
every row reads the statuses once, ~20s into the job.

If a bare push is right here (WIP branch, docs-only, no PR yet, or you already
attested), append \` # attest:ack reason=...\` and re-run.

Reference: scripts/ci/README.md
EOF
exit 1
