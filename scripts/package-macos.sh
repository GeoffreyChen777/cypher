#!/usr/bin/env bash
# macOS packaging: build the release binary for the host arch and produce
#   target/package/cypher-<version>-macos-<arch>.dmg          (user download)
#   target/package/cypher-<version>-macos-<arch>-app.tar.gz   (auto-updater)
# containing Cypher.app (unsigned unless CODESIGN_IDENTITY is set).
#
# Usage: scripts/package-macos.sh
#        scripts/package-macos.sh --icon-only /path/to/cypher.icns
# Env:   CODESIGN_IDENTITY="Developer ID Application: …" to sign the bundle.
#        NOTARY_KEY_PATH + NOTARY_KEY_ID + NOTARY_ISSUER_ID — App Store Connect
#        API key (.p8) for notarization; all three set → notarize + staple the
#        app and the dmg, which removes the Gatekeeper warning entirely.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(uname -m)" # arm64 on Apple silicon runners
OUT_DIR="$ROOT/target/package"
APP="$OUT_DIR/Cypher.app"
DMG="$OUT_DIR/cypher-$VERSION-macos-$ARCH.dmg"
APP_TARBALL="$OUT_DIR/cypher-$VERSION-macos-$ARCH-app.tar.gz"

cd "$ROOT"
# Reject stale/copied artwork before building or signing an app. Near-opaque
# pixels make macOS 26 inset the artwork on a second, gray icon backplate.
xcrun swift "$ROOT/scripts/macos-icon.swift" check \
  "$ROOT/dist/cypher.png" "$ROOT/dist/macos/icon-1024.png"

# Shared by the release bundle and the fast icon regression/preview path.
# Use an owned temporary directory, not the existing signed app or iconset.
package_icon() (
  output="$1"
  scratch="$(mktemp -d "${TMPDIR:-/tmp}/cypher-icon.XXXXXX")"
  trap 'rm -rf "$scratch"' EXIT
  iconset="$scratch/cypher.iconset"
  mkdir -p "$iconset"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ROOT/dist/macos/icon-1024.png" \
      --out "$iconset/icon_${size}x${size}.png" >/dev/null
    retina=$((size * 2))
    if [[ "$retina" -eq 1024 ]]; then
      cp "$ROOT/dist/macos/icon-1024.png" "$iconset/icon_${size}x${size}@2x.png"
    else
      sips -z "$retina" "$retina" "$ROOT/dist/macos/icon-1024.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    fi
  done
  iconutil -c icns "$iconset" -o "$output"
)

if [[ "${1:-}" == "--icon-only" && $# -eq 2 ]]; then
  package_icon "$2"
  echo "packaged icon: $2"
  exit 0
elif [[ $# -ne 0 ]]; then
  echo "Usage: $0 [--icon-only /path/to/cypher.icns]" >&2
  exit 2
fi

# Default features (desktop UI): macOS always ships the headed build.
cargo build --release --locked -p cypher

rm -rf "$APP" "$DMG" "$APP_TARBALL"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 755 "$ROOT/target/release/cypher" "$APP/Contents/MacOS/cypher"
sed "s/__VERSION__/$VERSION/" "$ROOT/dist/macos/Info.plist" >"$APP/Contents/Info.plist"

# Generate every standard/Retina size from the alpha-normalized macOS variant.
# Do not copy the shared PNG here: preserve its artwork, but normalize its
# near-opaque face and transparent background with scripts/macos-icon.swift.
package_icon "$APP/Contents/Resources/cypher.icns"

if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  # Hardened runtime + secure timestamp are both notarization requirements.
  # (No --deep: Apple deprecated it; the bundle is a single Mach-O anyway.)
  codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
else
  # Ad-hoc signature so the app launches on Apple silicon (Gatekeeper still
  # requires right-click → Open on first launch without notarization).
  codesign --deep --force --sign - "$APP"
fi

# notarize <path>: submit to Apple and wait for the verdict. A rejection may
# still exit 0 depending on the notarytool version — the `stapler staple` that
# follows each call has no ticket to attach then, and fails the build for us.
notarize() {
  xcrun notarytool submit "$1" \
    --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" \
    --issuer "$NOTARY_ISSUER_ID" --wait
}
NOTARIZE=false
[[ -n "${NOTARY_KEY_PATH:-}" && -n "${NOTARY_KEY_ID:-}" && -n "${NOTARY_ISSUER_ID:-}" ]] && NOTARIZE=true

if $NOTARIZE; then
  # Staple the bundle BEFORE tarring it: the auto-updater swaps the .app with
  # no dmg involved, so the tarball copy must carry its own ticket to pass
  # Gatekeeper offline.
  ZIP="$OUT_DIR/cypher-notarize.zip"
  ditto -c -k --keepParent "$APP" "$ZIP"
  notarize "$ZIP"
  rm -f "$ZIP"
  xcrun stapler staple "$APP"
fi

# The auto-updater artifact.
tar -czf "$APP_TARBALL" -C "$OUT_DIR" Cypher.app
echo "packaged: $APP_TARBALL"

# The dmg presents the classic drag-into-Applications layout over the
# ascii-hands artwork (committed renders from scripts/dmg-background.py).
# dmgbuild writes the .DS_Store (background, icon view, icon positions)
# directly — no Finder scripting, so it also works on headless CI runners.
python3 -c 'import dmgbuild' 2>/dev/null ||
  python3 -m pip install --quiet --user dmgbuild 2>/dev/null ||
  python3 -m pip install --quiet --user --break-system-packages dmgbuild

# Pair the 1x/2x background renders into a hidpi tiff so the artwork stays
# crisp on retina displays.
BG_TIFF="$OUT_DIR/dmg-background.tiff"
tiffutil -cathidpicheck "$ROOT/dist/macos/dmg-background.png" \
  "$ROOT/dist/macos/dmg-background@2x.png" -out "$BG_TIFF" >/dev/null 2>&1

APP="$APP" DMG="$DMG" BG_TIFF="$BG_TIFF" python3 - <<'PY'
import os
import dmgbuild

app = os.environ["APP"]
dmgbuild.build_dmg(
    filename=os.environ["DMG"],
    volume_name="Cypher",
    settings={
        "format": "UDZO",
        "files": [app],
        "symlinks": {"Applications": "/Applications"},
        "icon": os.path.join(app, "Contents/Resources/cypher.icns"),
        "background": os.environ["BG_TIFF"],
        "show_status_bar": False,
        "show_tab_view": False,
        "show_toolbar": False,
        "show_pathbar": False,
        "show_sidebar": False,
        "default_view": "icon-view",
        # Window and icon geometry must match scripts/dmg-background.py.
        "window_rect": ((200, 120), (660, 400)),
        "icon_size": 104,
        "text_size": 12,
        "icon_locations": {"Cypher.app": (165, 195), "Applications": (495, 195)},
    },
)
PY
rm -f "$BG_TIFF"
if $NOTARIZE; then
  notarize "$DMG"
  xcrun stapler staple "$DMG"
fi
echo "packaged: $DMG"
