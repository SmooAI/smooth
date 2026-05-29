#!/usr/bin/env bash
#
# build-release-notes.sh — produce a self-serve install/upgrade GitHub
# Release body for SmooAI/smooth.
#
# Usage:
#   scripts/build-release-notes.sh <version> <changelog-path>
#
# Examples:
#   scripts/build-release-notes.sh 0.13.7 CHANGELOG.md > /tmp/notes.md
#   gh release edit v0.13.7 --notes-file /tmp/notes.md -R SmooAI/smooth
#
# Output structure (writes to stdout, never to a file):
#   ## Install — copy-paste blocks for every install channel
#   ## Upgrade — one-liner per install path
#   ## What's new — extracted from CHANGELOG.md for the matching version
#   ## Downloads — actual asset filenames from the GH Release (queried via gh)
#                  with a fallback to the workflow's standard naming
#
# Wired into .github/workflows/release.yml — the release job calls this
# script and passes the output to softprops/action-gh-release via
# body_path. Pearl th-release-notes.

set -euo pipefail

VERSION="${1:?usage: $0 <version> <changelog-path>}"
CHANGELOG_PATH="${2:?usage: $0 <version> <changelog-path>}"
TAG="v${VERSION}"

# ---- 1. Install ------------------------------------------------------------

cat <<EOF
## Install

**Homebrew (recommended — macOS + Linux):**

\`\`\`bash
brew install SmooAI/tools/th
\`\`\`

**\`curl | sh\`:**

\`\`\`bash
curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh
\`\`\`

**Build from source (Cargo):**

\`\`\`bash
git clone https://github.com/SmooAI/smooth.git
cd smooth
cargo install --path crates/smooth-cli
\`\`\`

EOF

# ---- 2. Upgrade ------------------------------------------------------------

cat <<EOF
## Upgrade

\`\`\`bash
# Homebrew
brew update && brew upgrade th

# curl|sh — re-run the installer (it overwrites in place)
curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh

# Cargo — from your cloned checkout
cd smooth && git pull && cargo install --path crates/smooth-cli --force
\`\`\`

EOF

# ---- 3. What's new ---------------------------------------------------------

cat <<EOF
## What's new

EOF

# Anchored CHANGELOG match: "## 0.13.7" on its own line, then everything
# up to the next "## N.N.N" line. The version line is unconsumed by the
# print rule so the section header doesn't get duplicated.
NOTES=$(awk -v ver="$VERSION" '
    $0 ~ "^## " ver "$" { found=1; next }
    found && /^## [0-9]+\./ { exit }
    found { print }
' "$CHANGELOG_PATH")

if [[ -z "${NOTES// }" ]]; then
    cat <<EOF
See [CHANGELOG.md](https://github.com/SmooAI/smooth/blob/main/CHANGELOG.md) for details — no \`## ${VERSION}\` section matched in the changelog at build time.

EOF
else
    printf '%s\n\n' "$NOTES"
fi

# ---- 4. Downloads ----------------------------------------------------------

cat <<EOF
## Downloads

| Platform | File name |
|---|---|
EOF

# Map an asset filename to a human-readable platform label. Tolerates
# both the legacy rust-target naming (v0.13.7 and earlier:
# th-aarch64-apple-darwin.tar.gz) and the new smooblue-style naming
# (v0.13.8+: th-macos-arm64.tar.gz). Pearl th-e32f60 renames; this
# helper bridges the changeover so retroactive rebuilds work cleanly.
label_for_asset() {
    case "$1" in
        *macos-arm64*|*aarch64-apple-darwin*)         echo "macOS (Apple Silicon)" ;;
        *macos-x86_64*|*x86_64-apple-darwin*)         echo "macOS (Intel)" ;;
        *linux-x86_64*|*x86_64-unknown-linux*)        echo "Linux (x86_64)" ;;
        *linux-arm64*|*aarch64-unknown-linux*)        echo "Linux (arm64)" ;;
        *windows*|*pc-windows*)                       echo "Windows (x86_64)" ;;
        *)                                            echo "Other" ;;
    esac
}

# Prefer the live asset list — that's the source of truth and survives
# naming changes without needing this script updated. Falls back to the
# current workflow's expected naming when gh isn't available (CI
# without the right token) or the release doesn't exist yet (running
# this script BEFORE the release is published, which the workflow does
# to seed the body before `gh release create`).
if command -v gh >/dev/null 2>&1 && gh release view "$TAG" -R SmooAI/smooth --json assets >/dev/null 2>&1; then
    # Sort so the rows render in a stable order regardless of the order
    # gh returns assets. Skip files without a recognisable extension —
    # the v0.13.7 release has both `th-aarch64-apple-darwin` (the raw
    # binary) AND `th-aarch64-apple-darwin.tar.gz`; the tarball is the
    # canonical download.
    gh release view "$TAG" -R SmooAI/smooth --json assets --jq '.assets[].name' \
        | grep -E '\.(tar\.gz|zip|deb)$' \
        | sort -u \
        | while read -r asset; do
            platform=$(label_for_asset "$asset")
            echo "| ${platform} | \`${asset}\` |"
        done
else
    # Fallback table — matches the asset names the release workflow
    # uploads. Keep in sync with .github/workflows/release.yml's build
    # job matrix (pearl th-e32f60 set these names).
    cat <<EOF
| macOS (Apple Silicon) | \`th-macos-arm64.tar.gz\` |
| Linux (x86_64) | \`th-linux-x86_64.tar.gz\` |
| Linux (arm64) | \`th-linux-arm64.tar.gz\` |
EOF
fi

# ---- 5. Footer -------------------------------------------------------------

cat <<EOF

---

[Source](https://github.com/SmooAI/smooth) · [README](https://github.com/SmooAI/smooth#readme) · [Homebrew tap](https://github.com/SmooAI/homebrew-tools) · [Pearls (work tracker)](https://github.com/SmooAI/smooth#what-is-smooth)
EOF
