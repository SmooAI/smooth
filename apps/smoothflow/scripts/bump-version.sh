#!/usr/bin/env bash
# Bump SmoothFlow's version: scripts/bump-version.sh 0.2.1
# Rewrites CFBundleShortVersionString AND CFBundleVersion in Info.plist (sed, not
# plutil — plutil drops the hand-maintained comments). The publish workflow reads
# the version back out of Info.plist; nothing else carries it.
set -euo pipefail
V="${1:?usage: bump-version.sh <x.y.z>}"
[[ "$V" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "error: '$V' is not x.y.z" >&2; exit 2; }
cd "$(dirname "${BASH_SOURCE[0]}")/.."
for key in CFBundleShortVersionString CFBundleVersion; do
    sed -i '' -E "/<key>$key<\/key>/{n;s|<string>[^<]*</string>|<string>$V</string>|;}" Info.plist
done
grep -A1 -E "CFBundle(ShortVersionString|Version)</key>" Info.plist
