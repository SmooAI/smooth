#!/usr/bin/env bash
# Make the just-built `th` win on PATH (pearl th-15866f).
#
# `pnpm install:th` cargo-installs to ~/.cargo/bin/th. The menu bar's
# "Install th CLI…" symlinks /usr/local/bin/th (or ~/.local/bin/th) at
# Big Smooth.app/Contents/Resources/bin/th. Those directories usually come
# FIRST on PATH, so a successful dev install silently keeps serving the older
# bundled binary — you then debug a stale `th` and conclude your fix didn't
# work. That is exactly what happened on pearl th-fd9d98.
#
# So: after installing, if PATH resolves `th` to something other than the
# cargo binary, repoint it. Only symlinks are touched — a real file that
# someone deliberately installed gets a warning, never a clobber.
#
# Opt out with SMOOTH_NO_DEV_LINK=1 (release/packaging flows should set it).
#
# Usage:
#   scripts/dev-link-th.sh
#   scripts/dev-link-th.sh --cargo-bin PATH --path-th PATH   # for tests

set -euo pipefail

CARGO_TH="${CARGO_HOME:-$HOME/.cargo}/bin/th"
PATH_TH=""
PATH_TH_SET=0

while [ $# -gt 0 ]; do
    case "$1" in
        --cargo-bin) CARGO_TH="$2"; shift 2 ;;
        --path-th) PATH_TH="$2"; PATH_TH_SET=1; shift 2 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

[ "${SMOOTH_NO_DEV_LINK:-0}" = "1" ] && exit 0

# Nothing built yet — not our problem to report.
[ -x "$CARGO_TH" ] || exit 0

# th-fc32d9: `smoo` is the same binary under its platform-CLI name
# (`smoo <resource> <verb>` == `th smoo <resource> <verb>`, argv[0] dispatch).
# Keep a smoo -> th symlink next to the cargo binary and next to wherever
# `th` resolves on PATH. Only ever writes a symlink; a regular file named
# `smoo` (Homebrew, a manual copy) is left alone.
link_smoo() {
    _dir="$1"
    [ -d "$_dir" ] || return 0
    _tgt="$_dir/smoo"
    if [ ! -e "$_tgt" ] || [ -L "$_tgt" ]; then
        ln -sfn "$CARGO_TH" "$_tgt" 2>/dev/null || true
    fi
}
link_smoo "$(dirname "$CARGO_TH")"

if [ "$PATH_TH_SET" -eq 0 ]; then
    PATH_TH="$(command -v th 2>/dev/null || true)"
fi
[ -n "$PATH_TH" ] && link_smoo "$(cd "$(dirname "$PATH_TH")" && pwd)"

# Not on PATH at all: the cargo bin dir isn't wired up. Say so once; don't
# invent a symlink in a directory the user never asked us to write to.
if [ -z "$PATH_TH" ]; then
    echo "note: \`th\` is not on PATH. Add ${CARGO_TH%/th} to PATH, or use the menu bar's \"Install th CLI…\"." >&2
    exit 0
fi

# Already the dev build (directly, or via a symlink we previously repointed).
resolved="$(cd "$(dirname "$PATH_TH")" && pwd)/$(basename "$PATH_TH")"
if [ "$resolved" = "$CARGO_TH" ] || [ "$(readlink "$PATH_TH" 2>/dev/null || true)" = "$CARGO_TH" ]; then
    exit 0
fi

if [ -L "$PATH_TH" ]; then
    old="$(readlink "$PATH_TH")"
    if ln -sfn "$CARGO_TH" "$PATH_TH" 2>/dev/null; then
        printf '\033[1;36m==> repointed %s\033[0m\n' "$PATH_TH"
        echo "      was: $old"
        echo "      now: $CARGO_TH"
        echo "    (the menu bar's \"Install th CLI…\" points it back at the app bundle)"
    else
        echo "warning: \`th\` on PATH is $PATH_TH -> $old (the app bundle), not your build at $CARGO_TH." >&2
        echo "         Could not repoint it. Re-run with sudo, or invoke $CARGO_TH directly." >&2
    fi
    exit 0
fi

# A real file, not a symlink — someone installed it on purpose (Homebrew,
# a manual copy). Repointing would destroy it, so just be loud.
echo "warning: \`th\` on PATH is $PATH_TH, not your build at $CARGO_TH." >&2
echo "         It is a regular file, so it was left alone. Test with $CARGO_TH," >&2
echo "         or remove/rename $PATH_TH to let the dev build win." >&2
