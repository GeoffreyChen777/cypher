# MCP settings

In **Settings → MCP**, use the device selector first, then **Add MCP**.
The server runs on that device, not necessarily the computer showing the UI.

## Input modes

- **HTTP:** server name, URL, OAuth / bearer token / no authentication, and
  optional headers as a JSON object. HTTPS is required except for loopback HTTP.
  Put credentials in token or header fields, not URL userinfo or query strings.
  After adding an OAuth server, click **Sign in** on its row.
- **stdio:** server name, executable, optional argument array, environment
  object and working directory. Executables and paths must exist on the
  selected host. Arguments are passed as an array, not split like a shell line.
- **Import JSON:** accepts `{"mcpServers": {...}}`, a map of named servers, or
  one `{ "url": ... }` / `{ "command": ... }` server with a name supplied in the
  form. Global `settings` and `imports` are not imported.

Example:

```json
{
  "mcpServers": {
    "docs": {
      "url": "https://example.com/mcp",
      "auth": "oauth"
    },
    "local-tools": {
      "command": "node",
      "args": ["/absolute/path/to/server.js"]
    }
  }
}
```

The first importer supports command, args, env, cwd, url, headers, auth,
bearerToken, bearerTokenEnv, oauth, disabled, lifecycle, protocolVersion,
requestTimeoutMs and type. Unsupported options are rejected explicitly.
OAuth options support clientId, clientSecret, scope, redirectUri and grantType.
Switching input mode clears the draft. JSON, token, headers, argument and
environment inputs are masked because they may contain credentials.

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
and its saved Cypher OAuth record: the hashed account directory under
`agent/mcp-oauth/`, plus the matching entry in `agent/cypher-mcp.keychain-db`
on macOS. It never searches/deletes from the login keychain, system Pi, or
another device. Other servers and the shared Cypher keychain remain intact.
It does **not** revoke authorization with the remote service.

Finish active runs on the selected device before deleting. Idle Pi sessions
are recycled to reload configuration. If credential cleanup fails, the server
configuration is kept for retry; the error explicitly warns that some login
data might already have been removed. Cleanup and config writing span
different stores and are not claimed to be one atomic transaction.

Older remote engines that do not support `AddMcpServers` / `RemoveMcpServer`
must be updated before these actions work there. Editing existing entries is
not yet part of the form; enable/disable and OAuth actions remain available.
