# Packaging

Inputs for the release packaging scripts: `cypher.png` (the shared app icon,
also embedded by `crates/ui`) and `macos/` (the `Info.plist` template, the
macOS icon variant and the dmg background).

## Linux

```sh
scripts/package-linux.sh            # release build (thin LTO, stripped)
PROFILE=debug scripts/package-linux.sh   # fast smoke package
```

Produces `target/package/cypher-<version>-linux-<arch>.tar.gz` containing:

- `cypher` — the headless CLI/engine binary (`--no-default-features`; no GPUI/X11/Wayland linkage)
- `install.sh` — installs the binary as `~/.local/bin/cypher` and runs `cypher setup`
  when a terminal is attached

The updater and `install.sh` still accept older archives that also carried a
desktop entry and icon; current packages ship only the two files above.

Release Linux packages are built inside Ubuntu 20.04 and guarded to import no
GLIBC symbol newer than 2.31. Run `scripts/check-linux-abi.sh <binary>` when
auditing a candidate outside the release workflow.

The release profile in the root `Cargo.toml` sets `lto = "thin"` and
`strip = "symbols"` for distribution builds.

## macOS

```sh
scripts/package-macos.sh    # → target/package/cypher-<version>-macos-<arch>.dmg
```

Builds the release binary on a macOS host (gpui needs Metal; no cross-build
from Linux), assembles `Cypher.app` (`packaging/macos/Info.plist` + icns), signs it
(ad-hoc unless `CODESIGN_IDENTITY` names a Developer ID), notarizes and staples
when `NOTARY_KEY_PATH`/`NOTARY_KEY_ID`/`NOTARY_ISSUER_ID` are set, and wraps it
in a dmg. The auto-update tarball contains `Cypher.app`. CI runs this on
`cypher-macos-v<version>-b<build>` tags (`.github/workflows/macos.yml`).

### Icon

The bundle icon comes from `packaging/macos/icon-1024.png`, the alpha-normalized
macOS variant, rather than the shared `packaging/cypher.png`. The shared artwork
already includes its outline and padding, but has near-opaque face pixels.
macOS 26 interprets that translucency as a foreground symbol, adds a gray
backplate, and shrinks the artwork inside it. The macOS variant restores the
face to alpha 255 and removes faint background specks without resizing,
cropping, redrawing, or changing visible RGB values. Actual edge antialiasing is
retained; older macOS versions keep the existing outline.

After changing the shared artwork, regenerate and check the macOS variant:

```sh
xcrun swift scripts/macos-icon.swift generate packaging/cypher.png packaging/macos/icon-1024.png
xcrun swift scripts/macos-icon.swift test
xcrun swift scripts/macos-icon.swift check packaging/cypher.png packaging/macos/icon-1024.png
```

This uses macOS ImageIO, without Pillow or additional packages. Packaging and
macOS CI both reject a stale or unnormalized variant. `package-macos.sh`
generates all five standard sizes and their Retina counterparts, then places
`cypher.icns` at `Cypher.app/Contents/Resources/cypher.icns`. To build or test
just the icon, without rebuilding Rust, touching an existing app, or signing:

```sh
bash scripts/package-macos.sh --icon-only /tmp/cypher.icns
bash scripts/tests/test-macos-icon.sh
```
