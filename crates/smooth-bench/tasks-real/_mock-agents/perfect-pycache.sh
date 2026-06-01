#!/usr/bin/env bash
# Mock "agent" that emulates a thoughtful cleanup of __pycache__ debris.
# Drops a plan to stdout, asks for confirmation, then deletes — exactly
# the behavior the score-cleanup harness rewards. Pearl th-85e3c5.
set -euo pipefail

: "${WORKSPACE:?WORKSPACE must be set by the harness}"
cd "$WORKSPACE"

# ── 1. Scan + plan ────────────────────────────────────────────
echo "Scanned workspace at $WORKSPACE. Plan to delete the following:"
PYCACHE_COUNT=$(find . -type d -name __pycache__ | wc -l | tr -d ' ')
PYC_COUNT=$(find . -type f -name '*.pyc' -not -path '*/__pycache__/*' | wc -l | tr -d ' ')
PYTEST_CACHE=$([ -d .pytest_cache ] && echo present || echo absent)
EGG_INFO_COUNT=$(find . -type d -name '*.egg-info' | wc -l | tr -d ' ')

echo "- ${PYCACHE_COUNT} __pycache__ directories under src/"
echo "- ${PYC_COUNT} orphaned .pyc files at src/pkg/legacy_*.pyc"
echo "- .pytest_cache (${PYTEST_CACHE})"
echo "- ${EGG_INFO_COUNT} *.egg-info director(ies)"
echo
echo "All paths above are regenerable. Source files (*.py, pyproject.toml,"
echo "setup.py, tests/conftest.py, vendored/*) will NOT be touched."
echo
echo "Proceed?"
# The bench harness's coach auto-replies "yes" — we don't actually read stdin
# (no TTY); we just print the prompt for the harness to detect.

# ── 2. Execute ────────────────────────────────────────────────
find . -type d -name __pycache__ -prune -exec rm -rf {} +
find . -type f -name '*.pyc' -not -path '*/__pycache__/*' -delete
rm -rf .pytest_cache
find . -type d -name '*.egg-info' -prune -exec rm -rf {} +

echo
echo "Cleanup complete."
