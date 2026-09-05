# Cypher 0.3.0 — Isolated Runtime and workbench customization

## What's new

- **Isolated Pi Runtime:** Pi 0.85.1 and Cypher's curated plugins ship as a
  separate, downloadable Runtime bundle (0.85.1.1), independent of system Pi
  and `~/.pi`.
- **Device-scoped settings:** configure providers, agents, commands, and MCP
  against an explicit device. Provider credentials use masked inputs and are
  never returned by listing APIs. Remote transport uses TLS, not end-to-end
  encryption.
- **Workbench appearance:** Default, Catppuccin, Nord, and Gruvbox themes;
  independent Chat, Terminal, Git, and Sidebar colors; separate light/dark
  palettes; font and spacing controls; and a lightweight RGB/HEX picker.
- **Git diffs:** switch between Unified and side-by-side layouts, with
  independent horizontal scrolling and selection on each side.
- **Desktop polish:** full-message copying, a new ASCII loading wordmark,
  clearer worktree icons, aligned rounded tabs, and corrected macOS icon
  opacity.
- **Linux lifecycle:** checksum-required installation and updates, safer
  extraction and atomic activation, and hardened user-service management.
- **Release safety:** validated immutable artifacts and checksums, with
  channel pointers promoted only after all platform artifacts are ready.

## Platforms and rollout

- macOS ARM64 desktop: DMG and application archive.
- Linux x86_64 and ARM64: headless CLI archives, requiring glibc 2.31 or newer.
- Pi Runtime 0.85.1.1: macOS ARM64, Linux x86_64, and Linux ARM64.
- Edge, landing page, and canonical www redirect deploy from the same main
  commit after the installer compatibility gate passes.
- **iOS is not included in this release.**

Release tag: `cypher-v0.3.0`. Existing chats and device configuration are
preserved. Runtime and application versions are independently numbered.
