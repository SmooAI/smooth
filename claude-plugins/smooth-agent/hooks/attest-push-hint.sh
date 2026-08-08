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
# Exit codes: 0 allow silently, 1 ask, 2 hard block (stderr goes back to the model).
#
# This shipped as `exit 1` on the theory that a nudge was enough, because attesting
# is often the WRONG call and the agent has to be able to say no. Measured
# 2026-08-08, that theory was wrong: of 18 Claude transcripts that ran a `git push`
# that day, 7 had this hook fire — its stderr is right there in the transcript — and
# ZERO ran attest.sh or used the ack below. The last 15 merged smooai PRs carried 0
# ci-attest statuses and paid full CI. An ask is something an agent running in auto
# mode approves for itself, so `exit 1` was a log line with extra steps.
#
# Hence 2. Saying no is still cheap and still supported — it just has to be said out
# loud, by appending ` # attest:ack reason=...`, which is one comment rather than a
# silent default.
#
# Background: scripts/ci/README.md in the consuming repo · pearls th-6578ee, th-22d257

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
# The `(-C|--git-dir|--work-tree) <arg>` alternative is load-bearing: those flags
# take a VALUE, so a naive `(-[^ ]+\s+)*push` lets the value eat the `push` token
# and `git -C /some/repo push` reads as "not a push". Caught in testing 2026-08-06.
echo "$CMD" | grep -qE '(^|[;&|]|\s)git\s+((-C|--git-dir|--work-tree)[[:space:]=]+[^ ]+\s+|-[^ ]+\s+)*push(\s|$)' || exit 0
echo "$CMD" | grep -qE '(--delete|--dry-run|-d\s|--tags)' && exit 0

# WHERE the push will actually happen. `.cwd` is the SESSION's directory, which is
# not where the command runs: a session sits in one repo for its whole life, so
# agents write `cd /path/to/other-repo && git push`. Resolving from `.cwd` alone
# silently missed exactly that — the common case — and the hook shipped that way
# (verified 2026-08-06: real `cd …/smooai && git push` from a smooth session
# exited 0, while the same push with cwd=smooai correctly asked).
#
# Precedence, last wins, mirroring how the shell would actually end up:
#   1. `git -C <path> push`  — explicit, beats everything
#   2. the LAST `cd <path>` in the chain
#   3. `.cwd`
resolve_dir() {
    local base target
    base=$(echo "$INPUT" | jq -r '.cwd // "."' 2>/dev/null)

    target=$(echo "$CMD" | sed -nE 's/.*git[[:space:]]+-C[[:space:]]+([^[:space:];&|]+).*/\1/p' | tail -1)
    [[ -z "$target" ]] && target=$(echo "$CMD" | sed -nE 's/.*(^|[;&|][[:space:]]*)cd[[:space:]]+([^[:space:];&|]+).*/\2/p' | tail -1)

    # Strip quotes an agent may have wrapped a path in, and expand a leading ~.
    target=${target%\"}; target=${target#\"}; target=${target%\'}; target=${target#\'}
    [[ "$target" == "~"* ]] && target="$HOME${target#\~}"

    [[ -z "$target" ]] && { echo "$base"; return; }
    [[ "$target" == /* ]] && { echo "$target"; return; }
    echo "$base/$target"
}

# Repo-agnostic by design: the hook fires only where the convention exists, so
# copying scripts/ci/ into another repo turns this on there with no extra wiring.
ROOT=$(cd "$(resolve_dir)" 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null)
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
exit 2
