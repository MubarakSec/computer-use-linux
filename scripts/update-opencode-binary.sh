#!/usr/bin/env bash
# Install the freshly built release binary as an atomically-swapped symlink so
# a running MCP server (opencode) never sees a half-written or "text file busy"
# binary. Point opencode.json at the symlink (computer-use-linux-current) once;
# afterwards this script is the only thing you need before restarting opencode.
set -euo pipefail

# Make sure rustup's cargo is found even from a shell that never sourced ~/.cargo/env.
if [[ -d "$HOME/.cargo/bin" && ":$PATH:" != *":$HOME/.cargo/bin:"* ]]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${1:-$HOME/.local/bin}"
SYMLINK_NAME="computer-use-linux-current"

cd "$REPO_DIR"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if [[ -z "$VERSION" ]]; then
    echo "could not read version from Cargo.toml" >&2
    exit 1
fi

echo "building release binary for computer-use-linux v${VERSION} ..."
cargo build --release
BUILT="$REPO_DIR/target/release/computer-use-linux"
TARGET="$BIN_DIR/computer-use-linux-${VERSION}"

mkdir -p "$BIN_DIR"
cp -f "$BUILT" "$TARGET"
chmod 755 "$TARGET"

# Atomic symlink swap: opencode keeps the running inode; the next MCP server
# spawn resolves the new binary.
ln -sfn "$TARGET" "$BIN_DIR/$SYMLINK_NAME"

echo
echo "installed: $TARGET"
echo "symlink:   $BIN_DIR/$SYMLINK_NAME -> $TARGET"
echo
echo "opencode.json should point at: $BIN_DIR/$SYMLINK_NAME"
echo "restart opencode (or its MCP servers) to pick up the new binary."
echo "old binaries are kept alongside it for one-command rollback, e.g.:"
echo "  ln -sfn '$BIN_DIR/computer-use-linux-<old-version>' '$BIN_DIR/$SYMLINK_NAME'"
