# Cypher 0.1.3 — Linux Compatibility Hotfix

The Linux binaries shipped in 0.1.2 were built on Ubuntu 24.04 and imported
`GLIBC_2.39` symbols, so they crashed on older Linux distributions (e.g.
Ubuntu 20.04/22.04, Debian 11/12) with errors like
`version 'GLIBC_2.39' not found`. This hotfix rebuilds both Linux artifacts
inside an Ubuntu 20.04 container so the max imported GLIBC version is 2.31,
restoring support for older Linux hosts.

## What changed

- **Linux binaries are now built against glibc 2.31 (Ubuntu 20.04).** The
  release pipeline's `linux` job runs inside an `ubuntu:20.04` container on
  both the x86_64 and aarch64 runners; the produced binaries import no GLIBC
  symbol newer than 2.31.
- **ABI is verified in CI before packaging.** A new
  `scripts/check-linux-abi.sh` inspects the built binary with `readelf` and
  fails the release if any imported GLIBC version exceeds the 2.31 baseline
  (it is wired into `scripts/package-linux.sh` and the workflow smoke step).
- **The smoke test now runs on glibc 2.31.** The extracted binary is executed
  directly inside the Ubuntu 20.04 container (`timeout 8 cypher headless`,
  expecting rc 124), so the exact host baseline that previously broke is the
  one that must keep it running for the full 8 seconds.
- **macOS is unchanged** — Apple-silicon dmg/app tarball, default headed
  build, ad-hoc signed and not notarized unless CI signing secrets are
  configured.

## Action needed for 0.1.2 Linux installs

If you installed a Linux artifact from 0.1.2 and it fails to start on your
distro, reinstall 0.1.3 — either rerun

```sh
curl -fsSL https://edge.letscypher.app/install.sh | sh
```

or download the matching `cypher-0.1.3-linux-{x86_64,aarch64}.tar.gz` from
<https://edge.letscypher.app> and run its `install.sh`.

## Tags & artifacts

Release tag: `cypher-v0.1.3` — artifacts: `cypher-0.1.3-linux-{x86_64,aarch64}.tar.gz`,
`cypher-0.1.3-macos-arm64.dmg`, `cypher-0.1.3-macos-arm64-app.tar.gz`.

The Linux artifacts build genuinely headless (`--no-default-features`: no
X11/Wayland/GPUI linkage) inside an Ubuntu 20.04 container, and CI verifies
both that max imported GLIBC <= 2.31 and that `cypher headless` stays up on a
clean glibc 2.31 host; macOS ships the default headed build.
