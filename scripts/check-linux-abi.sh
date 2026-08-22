#!/usr/bin/env bash
# Verify a Linux ELF binary does not import GLIBC symbols newer than a
# baseline version. Linux release artifacts are built inside an Ubuntu 20.04
# container (glibc 2.31); this script proves the max imported GLIBC version
# stays <= baseline, so the binary also runs on older Linux hosts.
#
# Usage: scripts/check-linux-abi.sh <binary> [baseline]
#   binary   path to the Linux ELF executable to inspect.
#   baseline max allowed GLIBC version (default 2.31 == Ubuntu 20.04).
#
# Exits 0 if every imported GLIBC_* version is <= baseline, otherwise 1.
# Exits 2 on tooling/usage problems (not a readable ELF, readelf missing,
# or no GLIBC version requirements found).
#
# Env: CHECK_ABI_READELF — override the readelf command (the shell tests use
# this to inject a fake readelf, so the parser/compare can be exercised
# without a real old host).

set -euo pipefail

BIN="${1:-}"
BASELINE="${2:-2.31}"
READELF="${CHECK_ABI_READELF:-readelf}"

if [[ -z "$BIN" || ! -f "$BIN" ]]; then
  echo "error: usage: $0 <binary> [baseline]" >&2
  exit 2
fi

if ! command -v "$READELF" >/dev/null 2>&1; then
  echo "error: readelf not found (install binutils)" >&2
  exit 2
fi

# Must be a readable Linux ELF: readelf -h fails on scripts/data/foreign ELFs.
if ! "$READELF" -h "$BIN" >/dev/null 2>&1; then
  echo "error: $BIN is not an ELF file readable by readelf" >&2
  exit 2
fi

# Collect every imported GLIBC version from the .gnu.version_r dynamic
# section. readelf --version-info prints lines like:
#   0x0010:   Name: GLIBC_2.2.5  Flags: none  Version: 2
# (GLIBC_PRIVATE and non-GLIBC names are ignored by the anchored pattern.)
read -r -a versions <<< "$(
  "$READELF" --version-info "$BIN" 2>/dev/null \
    | sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' \
    | sort -u -V | tr '\n' ' '
)"

if [[ ${#versions[@]} -eq 0 ]]; then
  echo "error: no GLIBC version requirements found in $BIN" >&2
  exit 2
fi

max="$(printf '%s\n' "${versions[@]}" | sort -V | tail -n1)"
echo "imported GLIBC versions: ${versions[*]}"
echo "max imported GLIBC: $max (baseline: $BASELINE)"

# Version-aware compare via sort -V (correct for multi-dot versions like
# 2.31 vs 2.2.5, which a lexical compare would get wrong). Fails iff the
# greatest of {max, baseline} is not the baseline, i.e. max > baseline.
if [[ "$(printf '%s\n' "$max" "$BASELINE" | sort -V | tail -n1)" != "$BASELINE" ]]; then
  echo "error: $BIN imports GLIBC_$max, newer than baseline GLIBC_$BASELINE" >&2
  exit 1
fi

echo "OK: max imported GLIBC $max <= baseline $BASELINE"
