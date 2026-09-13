# Cypher 0.3.5 — Guided Linux setup, private Unix IPC and reliable model catalogs

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
  confirmation and Cypher-private OAuth cleanup.
- Improve Runtime installation progress, device-specific missing-Runtime
  guidance, and model cache invalidation after configuration changes.
- **Fix an intermittent empty provider model catalog.** Serialize SDK-initiated
  and explicit refreshes within each provider helper instance so an awaited
  read cannot be superseded by a background refresh and return an incomplete
  snapshot. Add deterministic queue/error tests and repeat the isolated
  provider lifecycle regression without real credentials or LLM spending.
- Refresh the website with the Pi workspace screenshot gallery, working cached
  image switching, and current fallback download links.
- Keep installer tests compatible with Ubuntu 20.04's Python 3.8 while retaining
  the glibc 2.31 baseline.

**Development configuration change:** `CYPHER_IPC_PORT` is no longer supported.
Remove it from local commands and service environments. Use `CYPHER_DATA_DIR`
to select a CLI/Engine instance; a UI with separate preferences can point to
that Engine using `CYPHER_ENGINE_DATA_DIR`. There is no legacy TCP fallback.

Runtime is **0.85.1.2**, with **Pi 0.85.1 / Node 24.19.0**. Its revision changes
because the provider helper changed; immutable 0.85.1.1 bundles are not modified.
Remote device connections continue through the existing TLS Edge relay;
this release does not add end-to-end encryption.

macOS ARM64 and Linux x86_64/ARM64 are included. No iOS release is published.
Existing chats and provider/MCP configuration are retained.
Release tag: `cypher-v0.3.5`. The 0.3.3 and 0.3.4 builds were not published;
their failed tags and all earlier public release artifacts remain unchanged.
