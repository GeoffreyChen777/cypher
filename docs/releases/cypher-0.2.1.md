# Cypher 0.2.1 — GUI toolchain PATH and a lighter dark theme

A patch for first-run installs from the desktop app, and a dark theme that
is no longer OLED-black.

## What's new

- **Install Pi / extensions from the welcome screen.** Finding `npm` and
  `pi` now uses the same PATH as the agent: login-shell snapshot (zshrc,
  including pnpm), Homebrew, and fnm/nvm/volta/bun. `pi install` inherits
  that PATH, so Dock/Finder launches no longer fail with `spawn npm ENOENT`.
- **Dark theme.** Surfaces, glass, and the terminal step up to charcoal
  (`#161616` / `#1c1c1c`) so the window reads less like a void.

## Tags & artifacts

Release tag: `cypher-v0.2.1` (immutable) — artifacts:
`cypher-0.2.1-linux-{x86_64,aarch64}.tar.gz`, `cypher-0.2.1-macos-arm64.dmg`,
`cypher-0.2.1-macos-arm64-app.tar.gz`.
