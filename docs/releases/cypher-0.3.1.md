# Cypher 0.3.1 — Fix Pi Runtime downloads

- Fix the default Runtime download URL to use Edge's public
  `/releases/runtimes/pi/` route. Cypher 0.3.0 incorrectly requested
  `/runtimes/pi/`, which returned an authentication error.
- Covers first installation and subsequent Runtime update checks.
- Keeps the isolated Runtime at **0.85.1.1**, containing **Pi 0.85.1**.
- Preserves existing chats, provider credentials, and device configuration.
- Includes all workbench customization and Linux lifecycle improvements from
  [0.3.0](cypher-0.3.0.md).

macOS ARM64 and Linux x86_64/ARM64 are included. iOS is not included.
Release tag: `cypher-v0.3.1`. The existing 0.3.0 artifacts are not overwritten.
