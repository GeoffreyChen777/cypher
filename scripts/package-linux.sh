#!/usr/bin/env bash
# Linux packaging: build the release binary and produce
#   target/package/cypher-<version>-linux-<arch>.tar.gz
# containing the binary, the .desktop entry, and the icon, plus an install.sh
# that drops them into ~/.local (XDG) paths.
#
# The binary is built headless (--no-default-features): the desktop UI/GPUI is
# not linked, so the artifact runs on a clean container with no X11/Wayland
# deps. `cypher headless`, login/logout/status/sync/daemon/update all still
# work; invoking with no subcommand prints a clear error.
#
# Release builds run inside an Ubuntu 20.04 container (glibc 2.31 baseline) so
# the artifact also runs on older Linux; scripts/check-linux-abi.sh proves the
# freshly built binary imports no GLIBC version newer than the baseline before
# anything is packaged.
#
# Usage: scripts/package-linux.sh
# Env:   PROFILE=debug for a fast unoptimized package (CI smoke); default release.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
PROFILE="${PROFILE:-release}"
ARCH="$(uname -m)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
OUT_DIR="$ROOT/target/package"
STAGE="$OUT_DIR/cypher-$VERSION-linux-$ARCH"
TARBALL="$STAGE.tar.gz"

cd "$ROOT"
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release --locked --no-default-features -p cypher
  BIN="$ROOT/target/release/cypher"
else
  cargo build --locked --no-default-features -p cypher
  BIN="$ROOT/target/debug/cypher"
fi

# Fail the package immediately if the binary imports GLIBC newer than the
# 2.31 / Ubuntu 20.04 baseline (that is what breaks older Linux hosts).
scripts/check-linux-abi.sh "$BIN"

rm -rf "$STAGE" "$TARBALL"
mkdir -p "$STAGE"
install -m 755 "$BIN" "$STAGE/cypher"
install -m 644 "$ROOT/dist/cypher.desktop" "$STAGE/cypher.desktop"
install -m 644 "$ROOT/dist/cypher.png" "$STAGE/cypher.png"

cat >"$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Install Cypher into ~/.local (no root needed).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
install -Dm755 "$HERE/cypher" "$HOME/.local/bin/cypher"
install -Dm644 "$HERE/cypher.desktop" "$HOME/.local/share/applications/cypher.desktop"
install -Dm644 "$HERE/cypher.png" "$HOME/.local/share/icons/hicolor/1024x1024/apps/cypher.png"
command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "$HOME/.local/share/applications" || true
echo "Installed. Make sure ~/.local/bin is on your PATH."
INSTALL
chmod 755 "$STAGE/install.sh"

tar -czf "$TARBALL" -C "$OUT_DIR" "$(basename "$STAGE")"
rm -rf "$STAGE"
echo "packaged: $TARBALL"
tar -tzf "$TARBALL"
