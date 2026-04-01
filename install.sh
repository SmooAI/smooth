#!/usr/bin/env sh
# Smooth installer — downloads the right binary for your platform.
# Usage: curl -fsSL https://raw.githubusercontent.com/SmooAI/smooth/main/install.sh | sh
set -e

REPO="SmooAI/smooth"
BIN_NAME="th"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    darwin) OS="apple-darwin" ;;
    linux) OS="unknown-linux-gnu" ;;
    *) echo "Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    arm64|aarch64) ARCH="aarch64" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

TARGET="${ARCH}-${OS}"

# Get latest release
echo "Detecting latest release..."
LATEST=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')

if [ -z "$LATEST" ]; then
    echo "Could not detect latest version. Using main branch."
    LATEST="main"
fi

echo "Installing Smooth ${LATEST} for ${TARGET}..."

# Download
URL="https://github.com/${REPO}/releases/download/v${LATEST}/th-${TARGET}.tar.gz"
TMPDIR=$(mktemp -d)
curl -fsSL "$URL" -o "${TMPDIR}/th.tar.gz"

# Extract
tar xzf "${TMPDIR}/th.tar.gz" -C "$TMPDIR"

# Install
mkdir -p "$INSTALL_DIR"
mv "${TMPDIR}/${BIN_NAME}" "${INSTALL_DIR}/${BIN_NAME}"
chmod +x "${INSTALL_DIR}/${BIN_NAME}"
rm -rf "$TMPDIR"

echo ""
echo "Smooth installed to ${INSTALL_DIR}/${BIN_NAME}"
echo ""

# Check PATH
if ! echo "$PATH" | grep -q "$INSTALL_DIR"; then
    echo "Add to your PATH:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
fi

echo "Get started:"
echo "  th auth login opencode-zen"
echo "  th up"
echo "  th tui"
