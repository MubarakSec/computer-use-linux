#!/usr/bin/env bash
# Install the freshly built release binary via a same-directory stage + atomic
# rename, then an atomically-swapped symlink, so a running MCP server
# (opencode) never sees a half-written or "text file busy" binary. Point
# opencode.json at the symlink (computer-use-linux-current) once; afterwards
# this script is the only thing you need before restarting opencode.
#
# Usage: scripts/update-opencode-binary.sh [bin-dir]   (default ~/.local/bin)
set -euo pipefail

# Make sure rustup's cargo is found even from a shell that never sourced ~/.cargo/env.
if [[ -d "$HOME/.cargo/bin" && ":$PATH:" != *":$HOME/.cargo/bin:"* ]]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found on PATH (looked in \$HOME/.cargo/bin and PATH)" >&2
    exit 1
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${1:-$HOME/.local/bin}"
SYMLINK_NAME="computer-use-linux-current"

cd "$REPO_DIR"

# Read the package version from the [package] section specifically.
VERSION="$(sed -n '/^\[package\]/,/^\[/{ s/^version = "\(.*\)"/\1/p }' Cargo.toml | head -1)"
if [[ -z "$VERSION" ]]; then
    echo "could not read version from Cargo.toml [package]" >&2
    exit 1
fi

echo "building release binary for computer-use-linux v${VERSION} ..."
cargo build --release --locked
BUILT="$REPO_DIR/target/release/computer-use-linux"
TARGET="$BIN_DIR/computer-use-linux-${VERSION}"
SYMLINK="$BIN_DIR/$SYMLINK_NAME"
STAGE=""

cleanup() {
    [[ -n "$STAGE" && -f "$STAGE" ]] && rm -f "$STAGE" || true
}
trap cleanup EXIT

# Never write in place to $TARGET: the running MCP server's exec path resolves
# through it. Stage in the same directory (same filesystem) and rename(2).
mkdir -p "$BIN_DIR"
STAGE="$(mktemp "$BIN_DIR/.computer-use-linux-${VERSION}.XXXXXX")"
cp -f "$BUILT" "$STAGE"
chmod 755 "$STAGE"
if ! "$STAGE" --version >/dev/null 2>&1; then
    echo "built binary failed --version smoke test; not installing" >&2
    exit 1
fi
mv -fT "$STAGE" "$TARGET"
STAGE=""

# Refuse to clobber a real file or real directory at the symlink path.
if [[ -e "$SYMLINK" && ! -L "$SYMLINK" ]]; then
    echo "refusing to replace $SYMLINK: it exists and is not a symlink" >&2
    echo "move it aside first, then re-run." >&2
    exit 1
fi
if [[ -d "$SYMLINK" ]]; then
    echo "refusing to replace $SYMLINK: it is a directory" >&2
    exit 1
fi

# Atomic symlink swap via temp symlink + rename: opencode keeps the running
# inode; the next MCP server spawn resolves the new binary.
TMP_LINK="$(mktemp -u "$BIN_DIR/.${SYMLINK_NAME}.XXXXXX")"
ln -s "$TARGET" "$TMP_LINK"
mv -fT "$TMP_LINK" "$SYMLINK"

# Post-swap verification.
[[ -x "$TARGET" ]] || { echo "target $TARGET is not executable" >&2; exit 1; }
[[ "$(readlink "$SYMLINK")" == "$TARGET" ]] || { echo "symlink $SYMLINK does not point at $TARGET" >&2; exit 1; }

echo
echo "installed: $TARGET"
echo "symlink:   $SYMLINK -> $TARGET"
echo
echo "opencode.json should point at: $SYMLINK"
echo "restart opencode (or its MCP servers) to pick up the new binary."
echo "old binaries are kept alongside it for one-command rollback, e.g.:"
echo "  ln -sfn '$BIN_DIR/computer-use-linux-<old-version>' '$SYMLINK'"
