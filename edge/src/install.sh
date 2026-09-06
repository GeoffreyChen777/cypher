#!/bin/sh
# Linux headless installer. Requires a release with per-artifact .sha256 files.
# curl -fsSL https://edge.letscypher.app/install.sh | sh
set -eu
umask 077

fail() { echo "cypher install: $*" >&2; exit 1; }
MINIMUM_SETUP_VERSION=0.3.3
setup=yes
case "${1:-}" in
  "") ;;
  --no-setup) setup=no ;;
  *) fail "usage: install.sh [--no-setup]" ;;
esac
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
for command in curl tar sha256sum cmp mv timeout awk grep; do
  command -v "$command" >/dev/null 2>&1 || fail "required command not found: $command"
done

# Fetch separately: a pipeline ending in tr used to hide curl failures. Reject
# path components / malformed pointers before constructing any local paths.
ver="$(curl --proto '=http,https' --proto-redir '=https' --connect-timeout 15 --max-time 30 -fsSL "$BASE/releases/latest.txt")"
case "$ver" in
  '' | *[!0-9.]* | .* | *. | *..*) fail "invalid release version" ;;
esac
[ "${#ver}" -le 64 ] || fail "invalid release version"
awk -v actual="$ver" -v minimum="$MINIMUM_SETUP_VERSION" 'BEGIN {
  a=split(actual,A,"."); b=split(minimum,B,".");
  for(i=1;i<=a || i<=b;i++) {if(A[i]+0>B[i]+0) exit 0; if(A[i]+0<B[i]+0) exit 1}
  exit 0
}' || fail "the release channel does not support guided setup yet; the existing installation was not changed"
file="cypher-$ver-linux-$arch.tar.gz"
root="${file%.tar.gz}"
app_root="$HOME/.cypher/app"
dest="$app_root/$ver"
mkdir -p "$app_root"
tmp="$(mktemp -d "$app_root/.install-XXXXXXXX")"
trap 'rm -rf "$tmp"; if [ -n "${command_tmp:-}" ]; then rm -f "$command_tmp"; fi' 0
trap 'exit 130' INT
trap 'exit 143' TERM

echo "Installing Cypher ${ver}…"
# Sidecar contains exactly the digest, not arbitrary sha256sum filenames.
expected="$(curl --proto '=http,https' --proto-redir '=https' --connect-timeout 15 --max-time 30 -fsSL "$BASE/releases/$file.sha256")"
case "$expected" in '' | *[!0-9a-fA-F]*) fail "invalid SHA-256 file" ;; esac
[ "${#expected}" -eq 64 ] || fail "invalid SHA-256 file"
curl --proto '=http,https' --proto-redir '=https' --connect-timeout 15 --max-time 300 -fsSL "$BASE/releases/$file" -o "$tmp/package.tar.gz"
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
timeout 10 "$tmp/unpacked/cypher" setup --help >/dev/null \
  || fail "release does not support guided setup; current installation unchanged"

if [ -e "$dest" ] || [ -L "$dest" ]; then
  if [ ! -L "$dest" ] && [ ! -L "$dest/cypher" ] && cmp -s "$tmp/unpacked/cypher" "$dest/cypher"; then
    : # Verified reinstall; avoid duplicate success/instruction output.
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

# One static, credential-free PATH fragment. Never reuse ~/.cypher/env, which
# is a systemd EnvironmentFile and is not shell code.
cat >"$tmp/shell-env.sh" <<'ENV'
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) export PATH="$HOME/.local/bin:$PATH" ;;
esac
ENV
path_env="$HOME/.cypher/shell-env.sh"
if [ ! -e "$path_env" ] && [ ! -L "$path_env" ]; then
  mv "$tmp/shell-env.sh" "$path_env"
fi
add_profile() {
  profile="$1"
  line='[ -f "$HOME/.cypher/shell-env.sh" ] && . "$HOME/.cypher/shell-env.sh"'
  # Leave symlink-managed dotfiles alone. The installed command is always
  # available by its absolute/tilde path.
  [ ! -L "$profile" ] || return 0
  if [ ! -f "$profile" ] || ! grep -Fqx "$line" "$profile"; then
    printf '\n# Cypher command path\n%s\n' "$line" >>"$profile" \
      || echo 'PATH setup skipped; the command is available at ~/.local/bin/cypher.'
  fi
}
# Do not source or attach an unrelated pre-existing file.
if [ ! -L "$path_env" ] && cmp -s "$path_env" "$tmp/shell-env.sh" 2>/dev/null; then
  path_safe=yes
elif [ ! -f "$tmp/shell-env.sh" ]; then
  path_safe=yes
else
  path_safe=no
fi
if [ "$path_safe" = yes ]; then
  case "${SHELL:-/bin/sh}" in
    */bash)
      add_profile "$HOME/.bashrc"
      if [ -f "$HOME/.bash_profile" ]; then add_profile "$HOME/.bash_profile"
      elif [ -f "$HOME/.bash_login" ]; then add_profile "$HOME/.bash_login"
      else add_profile "$HOME/.profile"; fi ;;
    */zsh) add_profile "$HOME/.zshrc"; add_profile "$HOME/.zprofile" ;;
    */fish)
      fish_root="${XDG_CONFIG_HOME:-$HOME/.config}"
      case "$fish_root" in /*) ;; *) fish_root="$HOME/.config" ;; esac
      fish_dir="$fish_root/fish/conf.d"
      if [ ! -e "$fish_dir/cypher.fish" ] && [ ! -L "$fish_dir/cypher.fish" ]; then
        mkdir -p "$fish_dir"
        cat >"$tmp/cypher.fish" <<'FISH'
if not contains -- "$HOME/.local/bin" $PATH
    set -gx PATH "$HOME/.local/bin" $PATH
end
FISH
        mv "$tmp/cypher.fish" "$fish_dir/cypher.fish"
      fi ;;
    *) add_profile "$HOME/.profile" ;;
  esac
fi

echo "✓ Cypher $ver installed"
# curl | sh leaves stdin carrying the script, not user input. Reopen the
# controlling terminal explicitly, without ever reading answers from the pipe.
if [ "$setup" = yes ] && ( : </dev/tty ) >/dev/null 2>&1; then
  rm -rf "$tmp"
  trap - 0 INT TERM
  exec "$app_root/current/cypher" setup </dev/tty
fi
echo 'Run: ~/.local/bin/cypher setup'
