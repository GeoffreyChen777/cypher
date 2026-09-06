# Cypher 0.3.3 — Guided Linux setup and private Unix IPC

- Introduce `cypher setup` for Linux: account connection (or local-only mode),
  verified Pi Runtime installation, background service setup and readiness
  checks. Add concise status and `cypher logs` commands.
- Hand interactive installations directly to setup using the controlling
  terminal. Non-interactive installation remains binary-only with one next
  step. Ask before using sudo for lingering; explain foreground-only operation.
- Replace fixed-port local RPC with private Unix sockets on Linux and macOS.
  Instances are scoped by OS user and canonical data directory, with private
  permissions, peer-UID checks, a versioned WebSocket handshake and device
  identity verification. Service names and updater restarts are instance-scoped.
- Switch desktop, CLI and the Pi bridge to Unix IPC. Keep client-local
  appearance, layout, tabs and composer preferences in the UI directory even
  when its Engine uses a separate data directory.
- Add device-scoped MCP creation (forms and JSON import) and deletion with
  confirmation and Cypher-private OAuth cleanup. Existing configurations,
  system Pi credentials and unrelated devices are not silently changed.
- Improve Runtime installation progress and device-specific missing-Runtime
  guidance. Refresh model caches after setup/configuration changes.
- Refresh the website with the Pi workspace screenshot gallery.

**Development configuration change:** `CYPHER_IPC_PORT` is no longer supported.
Remove it from local commands and service environments. Use `CYPHER_DATA_DIR`
to select a CLI/Engine instance; a UI with separate preferences can point to
that Engine using `CYPHER_ENGINE_DATA_DIR`. There is no legacy TCP fallback.

Runtime remains **0.85.1.1**, with **Pi 0.85.1 / Node 24.19.0**.
Remote device connections continue through the existing TLS Edge relay;
this release does not add end-to-end encryption.

macOS ARM64 and Linux x86_64/ARM64 are included. No iOS release is published.
Existing chats and provider/MCP configuration are retained.
Release tag: `cypher-v0.3.3`; earlier release artifacts are not overwritten.
