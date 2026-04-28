#!/usr/bin/env bash
# Build smooth-dolt-launcher: tiny C wrapper that resets signal mask
# + closes inherited fds + setsid before exec'ing smooth-dolt. See
# c/smooth-dolt-launcher/launcher.c for the full rationale.
#
# Output: target/release/smooth-dolt-launcher (ships alongside
# smooth-dolt and th).

set -euo pipefail

cd "$(dirname "$0")/.."

mkdir -p target/release

CFLAGS="${CFLAGS:--O2 -Wall -Wextra -Werror -fstack-protector-strong}"
CC="${CC:-cc}"

echo "==> Building smooth-dolt-launcher"
"$CC" $CFLAGS -o target/release/smooth-dolt-launcher c/smooth-dolt-launcher/launcher.c
SIZE=$(wc -c < target/release/smooth-dolt-launcher | tr -d ' ')
echo "==> Built smooth-dolt-launcher (${SIZE} bytes)"
