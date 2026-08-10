#!/usr/bin/env bash
# Run ponytail's statusline and ours, on one line, for people using both.
#
# Claude Code allows exactly ONE `statusLine` command in settings.json, and
# ponytail's installer only sets it when the key is absent — so whoever installs
# second silently loses. This wrapper is the "both" answer: point `statusLine` at
# this script instead of either one.
#
# Each statusline reads the SAME stdin payload, so we capture it once and feed a
# copy to each. Neither is required: a missing ponytail just yields our segment,
# and a failing segment is dropped rather than surfacing an error on every render.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INPUT="$(cat 2>/dev/null || true)"

# Newest installed ponytail, since the plugin cache keeps every version it has
# seen and the glob would otherwise be ordered by name, not recency.
PONYTAIL=""
for candidate in "$HOME"/.claude/plugins/cache/ponytail/ponytail/*/hooks/ponytail-statusline.sh; do
    [ -f "$candidate" ] || continue
    [ -z "$PONYTAIL" ] || [ "$candidate" -nt "$PONYTAIL" ] && PONYTAIL="$candidate"
done

segment() { # script
    [ -n "$1" ] && [ -f "$1" ] || return 0
    printf '%s' "$INPUT" | bash "$1" 2>/dev/null || true
}

LEFT="$(segment "$PONYTAIL")"
RIGHT="$(segment "$HERE/smooth-statusline.sh")"

if [ -n "$LEFT" ] && [ -n "$RIGHT" ]; then
    printf '%s %s' "$LEFT" "$RIGHT"
else
    printf '%s%s' "$LEFT" "$RIGHT"
fi
exit 0
