# Documentation

[README](../README.md) introduces the product and [ARCHITECTURE.md](../ARCHITECTURE.md)
explains how it works. Everything else is grouped here by who reads it.

## Features — using Cypher

- [Linux setup](features/linux-setup.md) — installing, updating and repairing a headless Linux device.
- [Chat appearance](features/chat-appearance.md) — chat fonts, colors, spacing and wide-screen mode.
- [Appearance colors](features/appearance-colors.md) — app themes and Terminal, Git and Sidebar color overrides.
- [Git diff layouts](features/git-diff.md) — unified and side-by-side diffs in the Git pane.
- [MCP settings](features/mcp-settings.md) — adding MCP servers to a device's Pi runtime.
- [iOS Files / Changes](features/ios-workspace-browser.md) — the iPhone's read-only file and change browser.

## Design — how a subsystem works and why

- [chat2 sync](design/chat2-sync.md) — session docs over the append-only chat2 row protocol.
- [Registry sync](design/registry-sync.md) — the workspace index as LWW rows instead of a CRDT.
- [Unix IPC](design/unix-ipc.md) — the private local engine transport, sockets and data directories.
- [Workspace layout](design/workspace-layout.md) — the desktop's sidebar plus tiled session workspace.
- [Syntax highlighting](design/syntax-highlighting.md) — the tree-sitter highlighter in `cypher-syntax`.
- [Notifications](design/notifications.md) — session-targeted mobile notifications.
- [Pi RPC harness](design/pi-rpc.md) — how the engine drives Pi over its native RPC protocol.

## Development — working on the code

- [Development](development/README.md) — repository map, crate layering, conventions and `scripts/check.sh`.
- [Local Edge development](development/local-edge.md) — running the Edge locally and pointing engines and iOS at it.
- [Edge](../apps/edge/README.md) — the Worker's layout, handler chain, conventions and tests.
- [Pi runtime](../pi-runtime/README.md) — the bundle spec: layout, test tiers and updating it.
- [Cross-language mirrors](../protocol/README.md) — code mirrored across Rust, TypeScript and Swift, and the shared test vectors.

## Operations — releasing and running the service

- [CI/CD operations](operations/ci-cd.md) — workflows, independent platform releases, credentials and recovery.
- [iOS release](operations/ios-release.md) — TestFlight preparation and the publication boundary.
- [Cloudflare billing and measurement](operations/cloudflare-billing.md) — measuring Edge cost, watching the bill, release retention.

## Releases

[releases/](releases/) holds the release notes, one file per version.
