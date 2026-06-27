#!/usr/bin/env bash
# install-skills.sh — symlink this repo's Claude skills into your local
# Claude config (~/.claude/skills) so Claude Code uses the repo's
# canonical, git-tracked version instead of an untracked local copy.
#
# Why symlink (not copy): the skill then lives in ONE place — this repo.
# Edits are versioned + shared, and "another session silently changed my
# untracked skill" can't happen. Idempotent; any existing non-symlink
# target is backed up first, never clobbered.
#
# Usage:
#   bash scripts/install-skills.sh           # link all repo skills
#   CLAUDE_SKILLS_DIR=/path bash scripts/install-skills.sh   # custom dest
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_DIR="$REPO_ROOT/.claude/skills"
DEST_DIR="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"

if [ ! -d "$SRC_DIR" ]; then
    echo "No skills to install — $SRC_DIR does not exist." >&2
    exit 0
fi

mkdir -p "$DEST_DIR"
linked=0
skipped=0

# Smooth Flow glyph vocabulary (matches gradient.rs / theme.rs): ✦ agent,
# ✓ success, ✗ failure, · system. Kept plain (no ANSI) for portability.
echo "✦ Smooth · install skills"

for skill in "$SRC_DIR"/*/; do
    [ -d "$skill" ] || continue
    name="$(basename "$skill")"
    src="${skill%/}"           # strip trailing slash
    link="$DEST_DIR/$name"

    # Already the correct symlink → nothing to do.
    if [ -L "$link" ] && [ "$(readlink "$link")" = "$src" ]; then
        echo "  · $name already linked"
        skipped=$((skipped + 1))
        continue
    fi

    # Existing file/dir/wrong-symlink → back it up, never overwrite.
    if [ -e "$link" ] || [ -L "$link" ]; then
        bak="$link.bak-$(date +%Y%m%d%H%M%S)"
        mv "$link" "$bak"
        echo "  · backed up existing $name → $(basename "$bak")"
    fi

    ln -s "$src" "$link"
    echo "  ✓ linked $name → $src"
    linked=$((linked + 1))
done

echo ""
echo "✓ done · $linked linked, $skipped already current · skills live in $SRC_DIR"
echo "  ❯ run /th-mail in Claude Code to bring this session online as a th agent"
