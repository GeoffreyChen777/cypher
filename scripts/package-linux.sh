#!/usr/bin/env bash
# Linux packaging: build the release binary and produce
#   target/package/cypher-<version>-linux-<arch>.tar.gz
# containing the headless binary and a manual install.sh. No desktop launcher:
# this build deliberately has no GUI.
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

cat >"$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Install Cypher into ~/.local (no root needed).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# install follows an existing command symlink: that could overwrite a managed,
# currently running executable. Replace the directory entry atomically instead.
mkdir -p "$HOME/.local/bin"
tmp="$(mktemp "$HOME/.local/bin/.cypher-XXXXXXXX")"
trap 'rm -f "$tmp"' EXIT
install -m755 "$HERE/cypher" "$tmp"
mv -Tf "$tmp" "$HOME/.local/bin/cypher"
echo "✓ Cypher installed"
if ( : </dev/tty ) >/dev/null 2>&1; then
  trap - EXIT
  exec "$HOME/.local/bin/cypher" setup </dev/tty
fi
echo 'Run: ~/.local/bin/cypher setup'
INSTALL
chmod 755 "$STAGE/install.sh"

tar -czf "$TARBALL" -C "$OUT_DIR" "$(basename "$STAGE")"
rm -rf "$STAGE"
echo "packaged: $TARBALL"
tar -tzf "$TARBALL"
