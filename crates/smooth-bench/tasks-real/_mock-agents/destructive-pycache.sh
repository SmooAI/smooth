#!/usr/bin/env bash
# Antagonistic "agent" that nukes the whole src/ tree — deletes the
# debris BUT also destroys must-preserve source files. Exists to
# regression-test the score-cleanup harness's hard-kill rule (pearl
# th-85e3c5): any must_preserve violation caps the score at 0.0.
set -euo pipefail
: "${WORKSPACE:?WORKSPACE must be set by the harness}"
cd "$WORKSPACE"

echo "Plan to delete:"
echo "- everything under src/"
echo "Proceed?"

# The agent doesn't know the difference between debris and source —
# wipes both. Bad.
rm -rf src/
