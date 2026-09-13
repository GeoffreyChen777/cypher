# Packaging

## Linux (implemented)

```sh
scripts/package-linux.sh            # release build (thin LTO, stripped)
PROFILE=debug scripts/package-linux.sh   # fast smoke package
```

Produces `target/package/cypher-<version>-linux-<arch>.tar.gz` containing:

- `cypher` — the headless CLI/engine binary (`--no-default-features`; no GPUI/X11/Wayland linkage)
- `cypher.desktop` — XDG desktop entry
- `cypher.png` — 1024×1024 Cypher app icon
- `install.sh` — installs into `~/.local/{bin,share/applications,share/icons}`

Release Linux packages are built inside Ubuntu 20.04 and guarded to import no
GLIBC symbol newer than 2.31. Run `scripts/check-linux-abi.sh <binary>` when
auditing a candidate outside the release workflow.

The release profile in the root `Cargo.toml` sets `lto = "thin"` and
`strip = "symbols"` for distribution builds.

## macOS

```sh
scripts/package-macos.sh    # → target/package/cypher-<version>-macos-<arch>.dmg
```

Builds the release binary, assembles `Cypher.app` (Info.plist + icns), ad-hoc
signs it (set `CODESIGN_IDENTITY` for a real Developer ID), and wraps it in a
dmg. The auto-update tarball contains `Cypher.app`.
CI runs this on tags
(`.github/workflows/release.yml`). The manual steps it automates, for reference
(run on a macOS host — gpui needs Metal; no cross-build from Linux):

1. Build the universal (or per-arch) binary:
   ```sh
   cargo build --release -p cypher --target aarch64-apple-darwin
   cargo build --release -p cypher --target x86_64-apple-darwin
   lipo -create -output cypher \
     target/aarch64-apple-darwin/release/cypher \
     target/x86_64-apple-darwin/release/cypher
   ```
2. Assemble the bundle:
   ```sh
   mkdir -p Cypher.app/Contents/{MacOS,Resources}
   cp cypher Cypher.app/Contents/MacOS/cypher
   sed "s/__VERSION__/$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')/" \
     dist/macos/Info.plist > Cypher.app/Contents/Info.plist
   ```
3. Icon: use `dist/macos/icon-1024.png`, the alpha-normalized macOS variant,
   rather than copying the shared `dist/cypher.png` directly. The shared artwork
   already includes its outline and padding, but has near-opaque face pixels.
   macOS 26 interprets that translucency as a foreground symbol, adds a gray
   backplate, and shrinks the artwork inside it. The macOS variant restores the
   face to alpha 255 and removes faint background specks without resizing,
   cropping, redrawing, or changing visible RGB values. Actual edge
   antialiasing is retained; older macOS versions keep the existing outline.

   After changing the shared artwork, regenerate and check the macOS variant:
   ```sh
   xcrun swift scripts/macos-icon.swift generate dist/cypher.png dist/macos/icon-1024.png
   xcrun swift scripts/macos-icon.swift test
   xcrun swift scripts/macos-icon.swift check dist/cypher.png dist/macos/icon-1024.png
   ```
   This uses macOS ImageIO, without Pillow or additional packages. Packaging and
   macOS CI both reject a stale or unnormalized variant. `package-macos.sh`
   generates all five standard sizes and their Retina counterparts, then places
   `cypher.icns` at `Cypher.app/Contents/Resources/cypher.icns`. To build or test
   just the icon, without rebuilding Rust, touching an existing app, or signing:
   ```sh
   bash scripts/package-macos.sh --icon-only /tmp/cypher.icns
   bash scripts/test-macos-icon.sh
   ```
4. Sign + notarize (required for distribution):
   ```sh
   codesign --deep --force --options runtime --sign "Developer ID Application: …" Cypher.app
   xcrun notarytool submit Cypher.zip --keychain-profile … --wait
   xcrun stapler staple Cypher.app
   ```
5. Ship as a `.dmg` (`hdiutil create -volname Cypher -srcfolder Cypher.app -ov -format UDZO Cypher.dmg`).
