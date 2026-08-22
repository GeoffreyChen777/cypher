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
3. Icon: generate `cypher.icns` from `dist/macos/icon-1024.png` (the macOS-shaped
   variant of the artwork — squircle mask, margins, and shadow pre-baked, since
   `sips` can't apply an alpha mask) and place it at
   `Cypher.app/Contents/Resources/cypher.icns`:
   ```sh
   mkdir cypher.iconset && sips -z 256 256 dist/macos/icon-1024.png --out cypher.iconset/icon_256x256.png
   iconutil -c icns cypher.iconset -o Cypher.app/Contents/Resources/cypher.icns
   ```
4. Sign + notarize (required for distribution):
   ```sh
   codesign --deep --force --options runtime --sign "Developer ID Application: …" Cypher.app
   xcrun notarytool submit Cypher.zip --keychain-profile … --wait
   xcrun stapler staple Cypher.app
   ```
5. Ship as a `.dmg` (`hdiutil create -volname Cypher -srcfolder Cypher.app -ov -format UDZO Cypher.dmg`).
