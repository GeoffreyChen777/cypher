#!/usr/bin/env bash
# Local/CI regression coverage; does not build Rust, sign, launch, or publish.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/cypher-icon-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

xcrun swift scripts/macos-icon.swift test
xcrun swift scripts/macos-icon.swift check dist/cypher.png dist/macos/icon-1024.png

# Explicit near-opaque regression fixture, independent of future brand artwork.
# Writing simple RGBA PNG fixtures needs only the Python standard library.
python3 - "$scratch" <<'PY'
from pathlib import Path
import struct
import sys
import zlib

def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

root = Path(sys.argv[1])
for name, alpha in (("opaque", 255), ("translucent", 253)):
    empty = bytes(1024 * 4)
    face = bytes(100 * 4) + bytes((17, 83, 191, alpha)) * 824 + bytes(100 * 4)
    rows = b"".join(b"\0" + (face if 100 <= y < 924 else empty) for y in range(1024))
    header = struct.pack(">IIBBBBB", 1024, 1024, 8, 6, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
    png += chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
    (root / f"{name}.png").write_bytes(png)
PY
xcrun swift scripts/macos-icon.swift check "$scratch/opaque.png" "$scratch/opaque.png"
if xcrun swift scripts/macos-icon.swift check "$scratch/opaque.png" "$scratch/translucent.png" \
    >"$scratch/negative.log" 2>&1; then
  echo "FAIL: translucent icon face passed the macOS icon gate" >&2
  exit 1
fi
grep -q 'stale or has unsafe alpha' "$scratch/negative.log"

# Never rewrite artwork used by iOS, the website, or in-app UI.
if xcrun swift scripts/macos-icon.swift generate dist/cypher.png dist/cypher.png \
    >"$scratch/same-path.log" 2>&1; then
  echo "FAIL: allowed overwriting the shared artwork" >&2
  exit 1
fi
grep -q 'do not overwrite the shared artwork' "$scratch/same-path.log"

# Disk round-trip, color-space preservation, idempotence, and path quoting.
xcrun swift scripts/macos-icon.swift generate dist/cypher.png "$scratch/generated icon.png"
xcrun swift scripts/macos-icon.swift check dist/macos/icon-1024.png "$scratch/generated icon.png"

# Exercise the actual release icon code, without replacing target/package's app.
bash scripts/package-macos.sh --icon-only "$scratch/Cypher icon.icns"
iconutil -c iconset "$scratch/Cypher icon.icns" -o "$scratch/extracted.iconset"
xcrun swift scripts/macos-icon.swift check dist/cypher.png \
  "$scratch/extracted.iconset/icon_512x512@2x.png"
python3 - "$scratch/extracted.iconset" <<'PY'
from pathlib import Path
import struct
import sys

root = Path(sys.argv[1])
expected = {}
for size in (16, 32, 128, 256, 512):
    expected[f"icon_{size}x{size}.png"] = size
    expected[f"icon_{size}x{size}@2x.png"] = size * 2
assert {p.name for p in root.iterdir()} == set(expected), "Incomplete icon family"
for name, size in expected.items():
    with (root / name).open("rb") as f:
        header = f.read(24)
    assert header[:8] == b"\x89PNG\r\n\x1a\n" and header[12:16] == b"IHDR", name
    assert struct.unpack(">II", header[16:24]) == (size, size), name
print("ICNS round-trip passed: all ten standard/Retina representations, 16–1024 pixels.")
PY
echo "macOS icon regressions passed."
