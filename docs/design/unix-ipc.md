# Private Unix IPC

Cypher's local Engine connections use **WebSocket over UnixStream**, on Linux
and macOS. The request/response/stream/cancel protocol is unchanged. The app
does not bind a local TCP RPC listener and does not fall back to one.
Remote device connections still use the existing outbound Edge relay.
Local handshakes must negotiate the `cypher.rpc.v1` WebSocket subprotocol;
unversioned/unsupported peers are rejected.

## Identity and location

The canonical data directory identifies an instance. The socket is:

```text
/tmp/cypher-ipc-<effective-uid>/<sha256(canonical-data-dir)[0:32]>/engine.sock
```

The short, deterministic namespace is intentionally independent of HOME path
length, network-mounted HOME, `TMPDIR`, `XDG_RUNTIME_DIR`, and the environment
inherited by SSH/systemd. No discovery sidecar or port allocator is needed.
Existing symlink aliases of a data directory resolve to the same instance.
Stop the Engine before moving its data directory.

Both private directories must belong to the current UID with no group/other
permissions. Newly created directories are 0700 and the socket is 0600.
Unexpected ownership, permissions, symlinks and non-socket files fail closed:
Cypher does not take over, chmod, or delete them. An unsafe pre-created namespace
can prevent startup; it cannot make Cypher trust another user's listener.

The Rust server and client also check the connected peer's UID through OS peer
credentials (`SO_PEERCRED` on Linux, the platform equivalent on macOS). After
connection, desktop/setup/sync verify the persisted device ID before treating
the peer as their Engine. Node's adapter verifies private directory/socket
ownership and permissions; the Rust server checks its peer UID.

This isolates different ordinary OS users, **not hostile processes sharing a
UID or an administrator/root**. Separate Cypher logins under one OS account are
not an OS security boundary.
This is IPC isolation, not a migration of existing chat/config file permissions;
keep each user's data directory under a suitably private filesystem directory.

## Commands and separate UI preferences

Normal use requires no IPC configuration:

```sh
cypher setup
cypher status
cypher status --verbose  # includes socket path
```

`CYPHER_DATA_DIR` selects the Engine for CLI commands. For development:

```sh
CYPHER_DATA_DIR=/tmp/my-engine cypher headless
CYPHER_DATA_DIR=/tmp/my-ui CYPHER_ENGINE_DATA_DIR=/tmp/my-engine cypher
```

`CYPHER_ENGINE_DATA_DIR` is UI-only: preferences remain under the UI data
directory while its boot/auth/restart operations target the selected Engine
directory. Without it, the UI and Engine share `CYPHER_DATA_DIR`.
There is deliberately no arbitrary socket-path environment override.

`CYPHER_IPC_PORT` is rejected, not silently ignored. Remove it from old local
launch commands/service environments. No legacy Engine migration, TCP
compatibility listener, or automatic fallback is implemented.

## Ownership and shutdown

The existing data-directory `InstanceLock` remains the single-owner authority.
The Engine acquires it before binding or recovering a stale socket. A live
listener is never unlinked. A refused, private stale socket may be removed only
under that lock, and only if its filesystem identity has not changed.

Listener cleanup compares device/inode before unlinking, so an old task cannot
delete a replacement listener. Shutdown cancels connection/request tasks and
waits for the listener task to exit before releasing the runtime. A foreign,
unresponsive or invalid endpoint is an error, not permission to start a second
embedded Engine.

Default data uses `cypher.service` / `ai.mvp-lab.cypher`. Other data directories
receive `cypher@<instance-key>.service` / `ai.mvp-lab.cypher.<instance-key>`.
CLI service operations and updater restarts use the same naming function.
Each Linux UID has its own systemd user manager.

## Pi bridge

The harness injects:

- `CYPHER_ENGINE_SOCKET`: this Engine's socket.
- `CYPHER_ENGINE_CLIENT_MODULE`: Engine-owned JavaScript client module.
- `CYPHER_CHAT_ID`: present only when the caller supplies a chat context.

Extensions can import the adapter without relying on Node's built-in
WebSocket supporting Unix sockets:

```js
import { pathToFileURL } from "node:url";
const { connectEngine } = await import(
  pathToFileURL(process.env.CYPHER_ENGINE_CLIENT_MODULE)
);
const client = await connectEngine();
try {
  const info = await client.call("EngineInfo");
  // for await (const event of client.subscribe(method, params, { signal })) …
} finally {
  client.close();
}
```

The module resolves `ws` from `PI_PACKAGE_DIR` in the isolated Runtime's locked
dependency tree. It supports request timeouts, stream cancellation, bounded
buffering and disconnect cleanup. It is embedded in the Engine binary and
written with the MCP keyring preload into a private per-process directory;
neither uses a shared writable `/tmp` filename. Installed Runtime bundles and
system Pi files are not modified.

## Validation

Rust tests cover private permissions, live/stale sockets, foreign filesystem
entries, replacement-safe cleanup, streams and disconnects. UI tests cover
embedding, concurrent boot, identity rejection and shutdown/restart ownership.
CLI regressions cover AF_UNIX diagnostics, multiple instances and removed TCP
configuration; Linux-only guided setup tests also exercise service lifecycle.

The Node adapter is tested against a fixture Unix server during Runtime
packaging. An additional real Node-to-Rust RPC test can be run with:

```sh
PI_PACKAGE_DIR=/path/to/pi-runtime/current/pi \
  cargo test -p cypher-rpc node_bridge_uses -- --ignored
```

Permission and native IPC tests do not substitute for a real multi-UID Linux
systemd session test.
