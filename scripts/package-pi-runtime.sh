#!/usr/bin/env bash
# Build the small-app / on-demand Pi Runtime artifact.
#
# The archive is deliberately separate from Cypher.app. It is downloaded on
# first use, checksum-verified, extracted under the Cypher data directory, and
# never touches the user's system Pi or ~/.pi.
#
# Usage:
#   scripts/package-pi-runtime.sh
#
# Optional:
#   PI_RUNTIME_VERSION=0.85.0.4
#   OUT_DIR=target/package

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC="$ROOT/dist/pi-runtime"
OUT_DIR="${OUT_DIR:-$ROOT/target/package}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) PLATFORM="macos-arm64"; ESBUILD="@esbuild/darwin-arm64" ;;
  Linux-x86_64) PLATFORM="linux-x86_64"; ESBUILD="@esbuild/linux-x64" ;;
  Linux-aarch64) PLATFORM="linux-aarch64"; ESBUILD="@esbuild/linux-arm64" ;;
  *) echo "unsupported Pi Runtime platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

PI_VERSION="$(node -p "require('$SPEC/package.json').dependencies['@earendil-works/pi-coding-agent']")"
RUNTIME_VERSION="${PI_RUNTIME_VERSION:-${PI_VERSION}.4}"
CYPHER_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
NAME="cypher-pi-runtime-$RUNTIME_VERSION-$PLATFORM"
STAGE="$OUT_DIR/$NAME"
ARCHIVE="$OUT_DIR/$NAME.tar.gz"
META="$OUT_DIR/$NAME.json"

rm -rf "$STAGE" "$ARCHIVE" "$META"
mkdir -p \
  "$STAGE/bin" \
  "$STAGE/node/lib/node_modules" \
  "$STAGE/npm" \
  "$STAGE/defaults" \
  "$STAGE/extensions"
cp "$SPEC/extensions/cypher-provider-auth.ts" "$STAGE/extensions/"
cp "$SPEC/provider-service.mjs" "$STAGE/"

# A private production dependency tree: Pi and every Cypher-curated extension
# are exact-version locked. npm runs only at build time, never on first launch.
cp "$SPEC/package.json" "$SPEC/package-lock.json" "$STAGE/npm/"
npm ci --prefix "$STAGE/npm" --omit=dev --ignore-scripts

# Keep only this artifact's native esbuild binary. Pi's shrinkwrap currently
# brings every platform package into some npm layouts (~285 MB uncompressed).
keep="${ESBUILD#@esbuild/}"
find "$STAGE/npm/node_modules" -type d -name '@esbuild' -print0 |
  while IFS= read -r -d '' esbuild_dir; do
    find "$esbuild_dir" -mindepth 1 -maxdepth 1 -type d \
      ! -name "$keep" -exec rm -rf {} +
  done

# Clipboard optional dependencies receive the same target-only pruning.
case "$PLATFORM" in
  macos-arm64) clipboard_keep='clipboard-darwin-arm64|clipboard-darwin-universal' ;;
  linux-x86_64) clipboard_keep='clipboard-linux-x64-gnu|clipboard-linux-x64-musl' ;;
  linux-aarch64) clipboard_keep='clipboard-linux-arm64-gnu|clipboard-linux-arm64-musl' ;;
esac
find "$STAGE/npm/node_modules" -type d -name '@mariozechner' -print0 |
  while IFS= read -r -d '' mario_dir; do
    find "$mario_dir" -mindepth 1 -maxdepth 1 -type d -name 'clipboard-*' |
      while read -r path; do
        [[ "$(basename "$path")" =~ ^($clipboard_keep)$ ]] || rm -rf "$path"
      done
  done

# Developer-only material is useful in a system Pi install but not required by
# the managed runtime. Source maps are also omitted; production stack traces
# still contain generated file and line locations.
find "$STAGE/npm/node_modules" -type d \( -name docs -o -name examples -o -name test -o -name tests \) \
  -prune -exec rm -rf {} +
find "$STAGE/npm/node_modules" -type f \( -name '*.map' -o -name 'CHANGELOG*' \) -delete

# Pi's code stays in the locked npm tree; this stable package-root link is what
# PI_PACKAGE_DIR points at after extraction.
ln -s "npm/node_modules/@earendil-works/pi-coding-agent" "$STAGE/pi"

# Bundle the matching Node executable and npm CLI. npm remains available to
# install an explicitly requested user plugin into the isolated agent dir.
NODE_EXE="$(node -p 'process.execPath')"
NPM_ROOT="$(npm root -g)/npm"
test -x "$NODE_EXE" || { echo "node executable not found: $NODE_EXE" >&2; exit 1; }
test -d "$NPM_ROOT" || { echo "npm package root not found: $NPM_ROOT" >&2; exit 1; }
cp "$NODE_EXE" "$STAGE/bin/node"
cp -R "$NPM_ROOT" "$STAGE/node/lib/node_modules/npm"

cat >"$STAGE/bin/pi" <<'SH'
#!/usr/bin/env sh
set -eu
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
ROOT="$(dirname "$HERE")"
exec "$HERE/node" "$ROOT/pi/dist/cli.js" "$@"
SH

cat >"$STAGE/bin/npm" <<'SH'
#!/usr/bin/env sh
set -eu
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
ROOT="$(dirname "$HERE")"
exec "$HERE/node" "$ROOT/node/lib/node_modules/npm/bin/npm-cli.js" "$@"
SH
chmod 755 "$STAGE/bin/node" "$STAGE/bin/pi" "$STAGE/bin/npm"

# Activation writes stable local package paths into the persistent agent
# settings. Keep a minimal default in the archive for format validation and
# forward-compatible repair tooling.
printf '{\n  "packages": []\n}\n' >"$STAGE/defaults/settings.json"

node - "$SPEC/package.json" "$STAGE/runtime.json" "$RUNTIME_VERSION" <<'NODE'
const fs = require("fs");
const spec = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const dependencies = { ...spec.dependencies };
const piVersion = dependencies["@earendil-works/pi-coding-agent"];
for (const name of Object.keys(dependencies)) {
  if (name.startsWith("@earendil-works/")) delete dependencies[name];
}
fs.writeFileSync(process.argv[3], JSON.stringify({
  version: process.argv[4],
  piVersion,
  plugins: dependencies,
}, null, 2) + "\n");
NODE

# Smoke-load the curated extension set from local paths. This catches missing
# peer/runtime dependencies without downloading or writing to the developer's
# real Pi configuration.
PROBE_AGENT="$(mktemp -d)"
trap 'rm -rf "$PROBE_AGENT"' EXIT
mkdir -p "$PROBE_AGENT/npm"
printf '{"name":"cypher-runtime-probe","private":true}\n' >"$PROBE_AGENT/npm/package.json"
node - "$STAGE/runtime.json" "$STAGE" "$PROBE_AGENT/settings.json" <<'NODE'
const fs = require("fs");
const path = require("path");
const manifest = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const packages = Object.keys(manifest.plugins)
  .sort()
  .map((name) => {
    const source = path.join(process.argv[3], "npm", "node_modules", name);
    return name === "pi-permission-control"
      ? { source, extensions: ["-index.ts"] }
      : source;
  });
const extensions = [
  path.join(process.argv[3], "extensions", "cypher-provider-auth.ts"),
];
fs.writeFileSync(process.argv[4], JSON.stringify({ packages, extensions }, null, 2) + "\n");
NODE
if ! PI_CODING_AGENT_DIR="$PROBE_AGENT" PI_PACKAGE_DIR="$STAGE/pi" \
  "$STAGE/bin/pi" --help >"$PROBE_AGENT/probe.log" 2>&1; then
  cat "$PROBE_AGENT/probe.log" >&2
  echo "Pi Runtime extension smoke test failed" >&2
  exit 1
fi
PI_PACKAGE_DIR="$STAGE/pi" CYPHER_PROVIDER_HELPER="$STAGE/provider-service.mjs" \
  "$STAGE/bin/node" --test "$SPEC/provider-service.test.mjs"
rm -rf "$PROBE_AGENT"
trap - EXIT

# The archive has one root directory; the installer validates every listed
# path, then extracts with --strip-components=1.
mkdir -p "$OUT_DIR"
tar -czf "$ARCHIVE" -C "$OUT_DIR" "$NAME"
SIZE="$(stat -f '%z' "$ARCHIVE" 2>/dev/null || stat -c '%s' "$ARCHIVE")"
SHA="$(shasum -a 256 "$ARCHIVE" 2>/dev/null | awk '{print $1}' || sha256sum "$ARCHIVE" | awk '{print $1}')"

node - "$STAGE/runtime.json" "$META" "$PLATFORM" "$(basename "$ARCHIVE")" "$SIZE" "$SHA" "$CYPHER_VERSION" <<'NODE'
const fs = require("fs");
const runtime = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const [platform, file, size, sha256, minimumCypherVersion] = process.argv.slice(4);
fs.writeFileSync(process.argv[3], JSON.stringify({
  ...runtime,
  minimumCypherVersion,
  files: {
    [platform]: { url: file, size: Number(size), sha256 },
  },
}, null, 2) + "\n");
NODE

rm -rf "$STAGE"
echo "packaged: $ARCHIVE"
echo "metadata: $META"
