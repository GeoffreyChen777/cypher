#!/bin/sh
# Linux headless installer. Requires a release with per-artifact .sha256 files.
# curl -fsSL https://edge.letscypher.app/install.sh | sh
set -eu
umask 077

fail() { echo "cypher install: $*" >&2; exit 1; }
BASE="${CYPHER_BASE_URL:-https://edge.letscypher.app}"
BASE="${BASE%/}"
case "$BASE" in *'@'* | *'?'* | *'#'* | *'\'*) fail "invalid release base URL" ;; esac
case "$BASE" in
  https://* | http://localhost:* | http://127.0.0.1:* | http://localhost | http://127.0.0.1) ;;
  *) fail "CYPHER_BASE_URL must use HTTPS (HTTP is allowed for loopback development)." ;;
esac
case "${HOME:-}" in /*) ;; *) fail "HOME must be an absolute path" ;; esac
case "$(uname -s)" in
  Linux) ;;
  Darwin) fail "on macOS, download the Cypher.app DMG instead." ;;
  *) fail "only GNU/Linux is supported." ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "unsupported architecture." ;;
esac
for command in curl tar sha256sum cmp mv timeout; do
  command -v "$command" >/dev/null 2>&1 || fail "required command not found: $command"
done

# Fetch separately: a pipeline ending in tr used to hide curl failures. Reject
# path components / malformed pointers before constructing any local paths.
ver="$(curl --proto '=http,https' --proto-redir '=https' --connect-timeout 15 --max-time 30 -fsSL "$BASE/releases/latest.txt")"
case "$ver" in
  '' | *[!0-9.]* | .* | *. | *..*) fail "invalid release version" ;;
esac
[ "${#ver}" -le 64 ] || fail "invalid release version"
file="cypher-$ver-linux-$arch.tar.gz"
root="${file%.tar.gz}"
app_root="$HOME/.cypher/app"
dest="$app_root/$ver"
mkdir -p "$app_root"
tmp="$(mktemp -d "$app_root/.install-XXXXXXXX")"
trap 'rm -rf "$tmp"; if [ -n "${command_tmp:-}" ]; then rm -f "$command_tmp"; fi' 0
trap 'exit 130' INT
trap 'exit 143' TERM

echo "downloading cypher $ver (linux-$arch)…"
# Sidecar contains exactly the digest, not arbitrary sha256sum filenames.
expected="$(curl --proto '=http,https' --proto-redir '=https' --connect-timeout 15 --max-time 30 -fsSL "$BASE/releases/$file.sha256")"
case "$expected" in '' | *[!0-9a-fA-F]*) fail "invalid SHA-256 file" ;; esac
[ "${#expected}" -eq 64 ] || fail "invalid SHA-256 file"
curl --proto '=http,https' --proto-redir '=https' --connect-timeout 15 --max-time 300 -fSL --progress-bar "$BASE/releases/$file" -o "$tmp/package.tar.gz"
actual="$(sha256sum "$tmp/package.tar.gz")"
actual="${actual%% *}"
expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"
[ "$actual" = "$expected" ] || fail "download checksum mismatch; current installation unchanged"

# These packages have a flat, known layout. Validate paths AND entry types
# before extraction: strip-components alone does not make hostile tar safe.
tar -tzf "$tmp/package.tar.gz" >"$tmp/members"
while IFS= read -r member; do
  case "$member" in
    "$root/" | "$root/cypher" | "$root/install.sh" | "$root/cypher.desktop" | "$root/cypher.png") ;;
    *) fail "unexpected archive member; current installation unchanged" ;;
  esac
done <"$tmp/members"
tar -tvzf "$tmp/package.tar.gz" >"$tmp/types"
awk 'substr($0,1,1) != "-" && substr($0,1,1) != "d" {exit 1}' "$tmp/types" \
  || fail "archive links or special files are not allowed"
mkdir "$tmp/unpacked"
tar -xzf "$tmp/package.tar.gz" -C "$tmp/unpacked" --strip-components=1 --no-same-owner
[ -f "$tmp/unpacked/cypher" ] && [ ! -L "$tmp/unpacked/cypher" ] && [ -x "$tmp/unpacked/cypher" ] \
  || fail "archive has no executable cypher binary"
timeout 10 "$tmp/unpacked/cypher" --help >/dev/null \
  || fail "binary cannot run on this host (GNU/Linux with glibc 2.31+ required)"

if [ -e "$dest" ] || [ -L "$dest" ]; then
  if [ ! -L "$dest" ] && [ ! -L "$dest/cypher" ] && cmp -s "$tmp/unpacked/cypher" "$dest/cypher"; then
    echo "cypher $ver already verified — relinking."
  else
    # Never overwrite a version directory, which might be in use. Incomplete
    # downloads now stay in .install-* and cannot masquerade as installations.
    fail "$dest differs from the verified release; move it aside before retrying"
  fi
else
  mv "$tmp/unpacked" "$dest"
fi

# GNU mv -T replaces the link itself, not the directory it points into. Keep
# the new link on the same filesystem for an atomic rename.
ln -s "$dest" "$tmp/current"
mv -Tf "$tmp/current" "$app_root/current"
mkdir -p "$HOME/.local/bin"
# ~/.local may be a separate filesystem; create the temporary link alongside
# its destination instead, and let mv replace only the link (not a directory).
command_tmp="$(mktemp "$HOME/.local/bin/.cypher-XXXXXXXX")"
rm -f "$command_tmp"
ln -s "$app_root/current/cypher" "$command_tmp"
mv -Tf "$command_tmp" "$HOME/.local/bin/cypher"

# A single implementation owns unit paths, escaping, environment capture and
# restarts. A set XDG_RUNTIME_DIR alone does not prove the user bus is usable.
service=manual
if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
  "$app_root/current/cypher" daemon install
  service=running
  user="${USER:-$(id -un)}"
  loginctl --no-ask-password enable-linger "$user" 2>/dev/null \
    || echo "note: enable start-at-boot with: sudo loginctl enable-linger $user"
else
  echo "note: no systemd user bus — use cypher headless under your process supervisor."
fi

echo ""
echo "✓ cypher $ver installed"
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) echo 'Add ~/.local/bin to PATH: export PATH="$HOME/.local/bin:$PATH"' ;;
esac
if [ "$service" = running ]; then
  echo "Engine started. Logs: journalctl --user -u cypher.service -f"
  echo "Optional sync: cypher daemon stop; cypher login; cypher daemon start"
else
  echo "Run cypher headless. For sync, run cypher login before starting it."
fi
echo "Pi is isolated: no system Pi or Claude CLI installation is needed."
echo "Connect this device from Cypher desktop to install Pi Runtime and configure Providers/MCP."
echo "Account login is optional; model providers still need their own credentials."
