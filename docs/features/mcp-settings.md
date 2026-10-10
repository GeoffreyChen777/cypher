# MCP settings

In **Settings → MCP**, use the device selector first, then **Add MCP**.
The server runs on that device, not necessarily the computer showing the UI.
MCP is served by Pi's built-in MCP support (Pi Runtime 1.0.0.2 and newer),
from that device's `pi-runtime/agent/mcp.json`; see
[Pi's MCP documentation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/mcp.md)
for the file format.

## Input modes

- **HTTP:** server name, URL, **OAuth / none** or **Bearer token**, and
  optional headers as a JSON object. HTTPS is required except for loopback HTTP.
  Put credentials in token or header fields, not URL userinfo or query strings.
  A bearer token is saved as the `Authorization` header. Without one, Pi signs
  in with OAuth only if the server requires it: click **Sign in** on its row.
- **stdio:** server name, executable, optional argument array, environment
  object and working directory. Executables and paths must exist on the
  selected host. Arguments are passed as an array, not split like a shell line.
- **Import JSON:** accepts `{"mcpServers": {...}}`, a map of named servers, or
  one `{ "url": ... }` / `{ "command": ... }` server with a name supplied in the
  form. Other top-level settings are not imported.

Example:

```json
{
  "mcpServers": {
    "docs": {
      "url": "https://example.com/mcp"
    },
    "local-tools": {
      "command": "node",
      "args": ["/absolute/path/to/server.js"]
    }
  }
}
```

The importer supports Pi's options: command, args, env, cwd, url, headers,
oauth, auth (`{"provider": …}`), enabled, timeout (seconds), exposure,
toolExposure, description and type (`stdio`, `http`, `streamable-http`; Pi does
not support legacy SSE). OAuth options are clientId, clientSecret, scope,
clientName, callbackPort, callbackUrl (loopback `http` only) and
authServerMetadataUrl. Unsupported options are rejected explicitly. Server names
use letters, digits, `_` and `-`; names that differ only in `-` and `_` are the
same server to Pi. pi-mcp-adapter fields from older Cypher versions (`auth:
"oauth" | "bearer" | false`, `bearerToken`, `bearerTokenEnv`, `disabled`,
`requestTimeoutMs`, `oauth.redirectUri`) are still accepted and saved in Pi's
shape. Switching input mode clears the draft. JSON, token, headers, argument
and environment inputs are masked because they may contain credentials.

By default Pi exposes MCP tools to the model through its `codemode` tool
(`"exposure": "codemode"`); set `exposure` or `toolExposure` to `deferred`,
`direct` or `hidden` per server or tool.

## OAuth sign-in on a remote runtime

Select the target device, then click **Sign in**. In the sign-in card, click
**Open authorization page** to open the provider in this computer's browser.
Approve access, then paste the **full callback URL** into the masked field and
click **Complete sign-in**. For a remote runtime, the browser may show a
localhost connection error: copy that address anyway. The callback listener
belongs to the remote runtime, not this computer. Local callbacks still
complete automatically when the browser can reach the listener.

Cypher binds the callback to the original attempt, OAuth state and redirect
URI. Credentials remain on the target runtime. The callback is not stored in
chat history. Remote handoff uses the existing authorized HTTPS/WSS relay
(TLS, **not E2EE**); the relay can see RPC contents.

Only one sign-in is active per runtime. **Cancel sign-in** stops its child;
closing the page cancels best-effort, and abandoned attempts expire after ten
minutes. Both the viewer's engine and the target engine must support interactive
MCP login; update older engines rather than falling back to a local login.

Sign-in runs Pi's `/mcp login <server>` on the target runtime, which stores
the tokens in its `agent/mcp-auth.json` and refreshes them itself. For servers
without dynamic client registration, import the provider's pre-registered
`oauth.clientId`, `scope` and loopback `callbackUrl` alongside the URL.
Starting sign-in preserves existing registration/refresh credentials instead
of implicitly signing out first. Pi also tries to open the authorization page
in a browser on the target runtime itself.

## Saving and safety

- Add up to 32 servers / 64 KiB in one operation. Existing names cause the whole
  addition to fail; they are never silently replaced.
- The engine atomically updates the selected device's
  `<data-dir>/pi-runtime/agent/mcp.json`, with private file permissions. Other
  servers and global settings survive. Malformed files or symlinks are refused
  rather than overwritten.
- Device selection is locked during saving; changing devices before saving
  discards the draft. A remote failure never falls back to writing locally.
- Remote configuration requires HTTPS/WSS relay transport (loopback development
  is allowed). This is TLS, **not end-to-end encryption**.
- Only add trusted configurations. stdio commands and dynamic secret helpers
  can execute when Pi loads configuration, including model/tool discovery.
- Saving is not a connection test. A `configured` badge does not verify server
  reachability. OAuth authentication remains a separate action.
- Lists and RPC responses contain metadata, not tokens, arguments or environment
  values. Successful additions invalidate discovery and recycle idle Pi
  sessions; running turns are not interrupted.

## Deleting a server

Click **Delete** on a server row, then confirm its name and target device.
Canceling does not change anything, and changing devices dismisses the pending
confirmation. The selector is locked while deletion is running.

Deletion removes that server's configuration (including inline credentials)
and its stored OAuth sign-in in `agent/mcp-auth.json`, under the same lock Pi
takes for that file. It never touches system Pi or another device. Other
servers' sign-ins remain intact. It does **not** revoke authorization with the
remote service. **Sign out** removes only the stored sign-in.

Finish active runs on the selected device before deleting. Idle Pi sessions
are recycled to reload configuration. If credential cleanup fails, the server
configuration is kept for retry; the error explicitly warns that some login
data might already have been removed. Cleanup and config writing span
different stores and are not claimed to be one atomic transaction.

Older remote engines that do not support `AddMcpServers` / `RemoveMcpServer`
must be updated before these actions work there. Editing existing entries is
not yet part of the form; enable/disable and OAuth actions remain available.

## Moving from pi-mcp-adapter

Pi Runtime 1.0.0.1 and older served MCP through the bundled pi-mcp-adapter.
Once a device runs a Runtime with Pi's built-in MCP and without the adapter,
its engine (at start and after each Runtime activation) rewrites the adapter
fields of `mcp.json` that Pi rejects or reads differently: `auth: "oauth" |
false` is dropped, `bearerToken` / `bearerTokenEnv` become the `Authorization`
header, `disabled` becomes `enabled: false`, `requestTimeoutMs` becomes
`timeout`, and a loopback `oauth.redirectUri` becomes `oauth.callbackUrl`.
Fields Pi ignores stay as they are.

Then it deletes the adapter's credential stores, which nothing reads any more:
the private `agent/cypher-mcp.keychain-db` (also removed from the keychain
search list) and its password file, `agent/mcp-oauth/`, and the adapter's
`mcp-cache.json`, `mcp-onboarding.json` and sign-in dump. The login keychain is
never touched. The tokens cannot carry over, so each OAuth server shows **not
signed in** until you click **Sign in** once.

If you install pi-mcp-adapter again, it replaces Pi's built-in MCP;
Settings → MCP then says so and offers no sign-in.
