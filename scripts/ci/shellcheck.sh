#!/usr/bin/env bash
# Run the pinned ShellCheck release over every tracked shell script.
#
#   bash scripts/ci/shellcheck.sh
#
# Downloads ShellCheck 0.11.0 once into $TMPDIR and verifies its checksum, so
# local runs and CI use the same version. SHELLCHECK=/path/to/shellcheck skips
# the download (the version is still printed).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
version=0.11.0
if [[ -z "${SHELLCHECK:-}" ]]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
      target=linux.x86_64
      sha=8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198 ;;
    Linux-aarch64)
      target=linux.aarch64
      sha=12b331c1d2db6b9eb13cfca64306b1b157a86eb69db83023e261eaa7e7c14588 ;;
    Darwin-arm64)
      target=darwin.aarch64
      sha=56affdd8de5527894dca6dc3d7e0a99a873b0f004d7aabc30ae407d3f48b0a79 ;;
    *) echo "unsupported shellcheck host" >&2; exit 1 ;;
  esac
  cache="${TMPDIR:-/tmp}/cypher-shellcheck-$version-$target"
  SHELLCHECK="$cache/shellcheck-v$version/shellcheck"
  if [[ ! -x "$SHELLCHECK" ]]; then
    mkdir -p "$cache"
    archive="$cache/shellcheck.tar.xz"
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
      --connect-timeout 15 --max-time 120 \
      "https://github.com/koalaman/shellcheck/releases/download/v$version/shellcheck-v$version.$target.tar.xz" \
      -o "$archive"
    python3 - "$archive" "$sha" <<'PY'
import hashlib, sys
assert hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest() == sys.argv[2], "shellcheck checksum mismatch"
PY
    tar -xJf "$archive" -C "$cache"
    rm -f "$archive"
  fi
fi
"$SHELLCHECK" --version | sed -n 2p
git ls-files -z -- 'scripts/*.sh' 'apps/edge/src/install.sh' \
  | xargs -0 "$SHELLCHECK" -S warning
