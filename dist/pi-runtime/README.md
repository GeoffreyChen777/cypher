# Cypher Pi Runtime

Cypher does not use or modify a system Pi installation. The desktop/headless
binary downloads a platform archive from:

```text
https://edge.letscypher.app/releases/runtimes/pi/manifest.json
```

The archive contains a pinned Node executable, Pi, npm, and Cypher's curated
plugins. Settings → Providers is the primary configuration surface. `/provider`,
`/provider add`, `/login <name>`, and `/logout <name>` open that same UI without
sending a chat message. A fresh installation can configure its first provider
before any model exists. At runtime:

- immutable versions live in `<device-data>/pi-runtime/versions/<version>`;
- `current` is an atomically replaced symlink;
- mutable settings, MCP config, OAuth state, and user-installed packages live
  in `<device-data>/pi-runtime/agent`;
- Pi subprocesses receive `PI_CODING_AGENT_DIR` and `PI_PACKAGE_DIR`, so
  `~/.pi` and a system `pi` executable are never consulted.

## Provider management

The first version supports NewAPI / OpenAI-compatible gateways using API keys.
`provider-service.mjs` uses the pinned Pi SDK's credential store and model
runtime, and the bundled NewAPI model builder. It shares the plugin's
`extension-settings/provider-newapi.json`, `auth.json`, and `models-store.json`;
it does not create a parallel credential store. Legacy NewAPI plugin commands
remain compatible. The native TUI auth bridge remains available outside RPC.

- Connect-and-add verifies an authenticated `/v1/models` response before saving.
- The engine sends keys through stdin, never process arguments or chat commands.
- The UI masks keys, disables copy/cut and undo for the secret field, and clears
  it on submission/dismissal. APIs never return stored keys.
- HTTPS is required except for loopback development endpoints. Redirects are
  refused and model responses are bounded; a changed URL requires a key again.
- Saved credentials and verified connectivity are distinct states. A failed
  refresh keeps the cached catalog and shows an error, not a connected badge.
- Mutations recycle idle Pi sessions and invalidate model discovery/picker caches.
  Existing chats and their selected model are not deleted or silently switched.
- A single device selector is pinned at the top of the Settings sidebar.
  Agents, Providers, Commands and MCP form the **Device settings** group and
  follow that selection; the individual pages do not repeat the selector.
  **Client settings** (appearance, notifications, shortcuts) and **Workspace**
  (device registry, archived sessions) are separate groups, not host-scoped.
  Provider commands opened from chat select that chat's host. Every request
  binds its target before dispatch; changing devices clears forms and rejects
  late results. Commands' menu visibility remains a client preference, not a
  host permission.
- Remote provider credentials use the existing same-account authorized relay.
  HTTPS/WSS is required (loopback development is allowed); this is **not E2EE**,
  and the relay can see RPC content. Keys are not automatically synchronized or
  returned by listing APIs. Each target's Runtime remains the credential owner.

The package script runs `provider-service.test.mjs` against the staged Runtime
and a local fixture endpoint, including the full credential lifecycle and
redaction checks. No real provider credentials are needed.

## Updating the bundle

1. Change exact dependency versions in `package.json`.
2. Run `npm install --package-lock-only --ignore-scripts` in this directory.
3. If Pi's version did not change, bump the bundle revision:

   ```bash
   PI_RUNTIME_VERSION=0.85.0.4 scripts/package-pi-runtime.sh
   ```

4. Test the generated archive under `target/package/`.

Tagged releases build all supported platforms, merge their metadata, upload
the archives first, and publish `runtimes/pi/manifest.json` last. Cypher checks
that manifest after startup and every six hours. Existing installations update
in staging, verify SHA-256 and `pi --version`, then switch `current`; older
version directories remain available for rollback.
