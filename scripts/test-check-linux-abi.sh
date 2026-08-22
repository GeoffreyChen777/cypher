#!/usr/bin/env bash
# Focused shell-level tests for scripts/check-linux-abi.sh — no real old
# Linux host or real ELF binaries needed. CHECK_ABI_READELF lets us inject a
# fake readelf feeding canned --version-info output, so the parser and the
# version comparison (sort -V) are exercised in isolation.
#
# Usage: scripts/test-check-linux-abi.sh
# Exits non-zero if any check fails.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/check-linux-abi.sh"

pass=0
fail=0
ok()   { printf 'ok   %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL %s\n' "$1"; fail=$((fail + 1)); }

# --- syntax check -----------------------------------------------------------
if bash -n "$SCRIPT"; then ok "bash -n $SCRIPT"; else bad "bash -n $SCRIPT"; fi

# --- fixtures ---------------------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
FAKE_BIN="$tmp/dummy" # content is never inspected (readelf is faked)
: >"$FAKE_BIN"

# make_fake_readelf <outfile> <glibc-versions...> — a readelf stand-in that
# answers `-h` with a plausible ELF header and `--version-info` with one
# version-requirement entry per given GLIBC version.
make_fake_readelf() {
  local f="$1"; shift
  {
    echo '#!/usr/bin/env bash'
    echo 'case "$1" in'
    echo '  -h) echo "ELF 64-bit LSB executable, x86-64, version 1 (SYSV)"; exit 0 ;;'
    echo '  --version-info)'
    echo "    echo \"Version needs section '.gnu.version_r' contains 1 entry:\""
    echo "    echo '  0x0000: Version: 1  File: libc.so.6  Cnt: 1'"
    local v
    for v in "$@"; do
      echo "    echo '  0x0010:   Name: $v  Flags: none  Version: 2'"
    done
    echo '    ;;'
    echo '  *) echo "fake readelf: unexpected args: $*" >&2; exit 2 ;;'
    echo 'esac'
  } >"$f"
  chmod +x "$f"
}

# run <fake_readelf> <baseline> <expected_rc> <label>
run() {
  local fake="$1" baseline="$2" want="$3" label="$4" got=0
  CHECK_ABI_READELF="$fake" "$SCRIPT" "$FAKE_BIN" "$baseline" >/dev/null 2>&1 \
    || got=$?
  if [[ "$got" == "$want" ]]; then ok "$label (rc=$got)"; else bad "$label (want rc=$want, got rc=$got)"; fi
}

# --- version comparison / parser cases --------------------------------------
make_fake_readelf "$tmp/ok"      GLIBC_2.2.5 GLIBC_2.31
run "$tmp/ok" 2.31 0 "max 2.31 == baseline 2.31 passes"

make_fake_readelf "$tmp/old"     GLIBC_2.2.5 GLIBC_2.17
run "$tmp/old" 2.31 0 "max 2.17 < baseline 2.31 passes (multi-dot sort -V)"

make_fake_readelf "$tmp/new"     GLIBC_2.2.5 GLIBC_2.39
run "$tmp/new" 2.31 1 "max 2.39 > baseline 2.31 fails (the 0.1.2 regression)"

make_fake_readelf "$tmp/newer"   GLIBC_2.34
run "$tmp/newer" 2.31 1 "max 2.34 > baseline 2.31 fails"

make_fake_readelf "$tmp/boundary" GLIBC_2.2.5 GLIBC_2.31
run "$tmp/boundary" 2.31 0 "boundary 2.31 with custom baseline passes"

# --- parser / tooling failure cases -----------------------------------------
make_fake_readelf "$tmp/none" someOTHER_1.0
run "$tmp/none" 2.31 2 "no GLIBC version requirements fails (rc=2)"

cat >"$tmp/notelf" <<'EOF'
#!/bin/sh
echo "definitely not an elf"
EOF
chmod +x "$tmp/notelf"
cat >"$tmp/fail_h" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  -h) echo "readelf: Error: '$2' is not an ELF file" >&2; exit 1 ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$tmp/fail_h"
run "$tmp/fail_h" 2.31 2 "non-ELF file fails (rc=2)"

run "$tmp/does-not-exist" 2.31 2 "missing readelf fails (rc=2)"

# --- usage case -------------------------------------------------------------
if "$SCRIPT" >/dev/null 2>&1; then bad "no arguments fails"; else ok "no arguments fails (rc=2)"; fi
if "$SCRIPT" "$tmp/does-not-exist" >/dev/null 2>&1; then
  bad "nonexistent binary fails"
else
  ok "nonexistent binary fails (rc=2)"
fi

# --- summary -----------------------------------------------------------------
echo
echo "check-linux-abi tests: $pass passed, $fail failed"
[[ "$fail" -eq 0 ]]
