#!/usr/bin/env bash
# Install smooth-dolt into ~/.cargo/bin/ (the same directory `cargo
# install --path …` uses) so it's on $PATH globally — the dolt
# binary discovery code first tries `smooth-dolt --help` via PATH,
# so dropping it next to `th` makes `th code` find it from ANY cwd
# (including ~/dev/smooai/smooai/ where the existing relative
# target/release/ lookup misses).
#
# Pearl th-92dac3.

set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)/target/release/smooth-dolt"
DEST="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin/smooth-dolt"

if [ ! -f "$SRC" ]; then
    echo "==> smooth-dolt not at $SRC — did you run build:smooth-dolt first?" >&2
    exit 1
fi

mkdir -p "$(dirname "$DEST")"

# Skip the copy if the destination is already byte-identical to the
# source. Avoids touching a hot binary that other processes (e.g. a
# running `th up`) may be mid-exec on, and keeps `install:th` cheap
# on hot reinstalls. cmp returns 0 when files match.
if [ -f "$DEST" ] && cmp -s "$SRC" "$DEST"; then
    echo "==> smooth-dolt already installed at $DEST (up-to-date)"
    exit 0
fi

cp "$SRC" "$DEST"
chmod +x "$DEST"
echo "==> Installed smooth-dolt to $DEST"
