#!/bin/sh
# Cypher (native) headless installer.
#
#   curl -fsSL https://edge.letscypher.app/install.sh | sh
#
# Installs the self-contained native binary (no runtime deps) to
# ~/.cypher/app, puts `cypher` on PATH, and runs it as a local-only systemd
# user service that survives reboots. Signing in is optional and enables sync
# after a restart. Re-running upgrades in place; existing state is preserved.
#
# The binary ships with production endpoints baked in: no CYPHER_EDGE_URL or
# client-id configuration needed. Overrides (if any) go in $data_root/env.
set -eu

# CYPHER_BASE_URL overrides the production edge.
BASE="${CYPHER_BASE_URL:-https://edge.letscypher.app}"

# --- data root ---------------------------------------------------------------
data_root="$HOME/.cypher"

# --- platform ---------------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux) plat=linux ;;
  Darwin)
    echo "cypher install: on macOS, download the desktop app instead:" >&2
    echo "  $BASE/releases/latest.txt → $BASE/releases/cypher-<version>-macos-arm64.dmg" >&2
    exit 1
    ;;
  *)
    echo "cypher install: unsupported OS '$os' — only Linux for now." >&2
    exit 1
    ;;
esac
case "$arch" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *)
    echo "cypher install: unsupported architecture '$arch'." >&2
    exit 1
    ;;
esac

# --- download ----------------------------------------------------------------
ver="$(curl -fsSL "$BASE/releases/latest.txt" | tr -d '[:space:]')"
[ -n "$ver" ] || { echo "cypher install: could not resolve latest version" >&2; exit 1; }
file="cypher-$ver-$plat-$arch.tar.gz"
app_root="$data_root/app"
dest="$app_root/$ver"

if [ -x "$dest/cypher" ]; then
  echo "cypher $ver already downloaded — relinking."
else
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  echo "downloading cypher $ver ($plat-$arch)…"
  curl -fSL --progress-bar "$BASE/releases/$file" -o "$tmp/$file"
  mkdir -p "$dest"
  tar -xzf "$tmp/$file" -C "$dest" --strip-components=1
fi

ln -sfn "$dest" "$app_root/current"
mkdir -p "$HOME/.local/bin"
ln -sf "$app_root/current/cypher" "$HOME/.local/bin/cypher"

# --- service -----------------------------------------------------------------
# The daemon is useful before auth: without a saved session it serves the local
# profile. Login only changes which profile the next daemon start selects.

service=manual
if command -v systemctl >/dev/null 2>&1 && [ -n "${XDG_RUNTIME_DIR:-}" ]; then
  mkdir -p "$HOME/.config/systemd/user"
  cat >"$HOME/.config/systemd/user/cypher.service" <<UNIT
[Unit]
Description=Cypher native headless engine
After=network-online.target
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
ExecStart=$app_root/current/cypher headless
Restart=on-failure
RestartSec=5
EnvironmentFile=-$data_root/env

[Install]
WantedBy=default.target
UNIT
  systemctl --user daemon-reload
  systemctl --user enable cypher
  systemctl --user restart cypher
  service=running
  # Keep the user manager (and the engine) running without an active login.
  loginctl enable-linger "$USER" 2>/dev/null \
    || sudo -n loginctl enable-linger "$USER" 2>/dev/null \
    || echo "warn: could not enable linger — the engine stops when you log out (run: sudo loginctl enable-linger $USER)"
else
  echo "warn: systemd user session not available — run the engine manually with: cypher headless"
fi

# --- agent CLIs ---------------------------------------------------------------
command -v claude >/dev/null 2>&1 || \
  echo "note: Claude Code CLI not found — install it with: curl -fsSL https://claude.ai/install.sh | bash"

case ":$PATH:" in
  *":$HOME/.local/bin:"*) path_hint="" ;;
  *) path_hint=' (add ~/.local/bin to your PATH)' ;;
esac

echo ""
echo "✓ cypher $ver installed$path_hint"
echo ""
case "$service" in
  running)
    echo "the engine is running with the new version (local-only unless sync is enabled)."
    echo "  systemctl --user status cypher    check the service"
    echo ""
    echo "optional sync (local sessions stay local):"
    echo "  systemctl --user stop cypher"
    echo "  cypher login"
    echo "  systemctl --user restart cypher"
    ;;
  manual)
    echo "next: run the local-only engine with \`cypher headless\`."
    echo "optional sync: run \`cypher login\` before starting the engine."
    ;;
esac
