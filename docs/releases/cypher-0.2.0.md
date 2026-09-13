# Cypher 0.2.0 — Pi, Extensions, and MCP

Pi is now the coding agent: first-run setup, a curated extensions list, a
Commands picker, and an MCP settings page with the same `/mcp-auth` path as
the Pi TUI.

## What's new

- **Pi-first setup.** After sign-in, Cypher offers Install Pi and the
  recommended extensions. Settings → Agents remains the place to toggle them
  later.
- **Commands.** Settings → Commands lists every slash command. Hide or show
  each one; defaults hide TUI-only skills, llama, NewAPI, compact-ui, and MCP
  slash commands.
- **MCP.** Settings → MCP lists servers from `~/.pi/agent/mcp.json`, with
  Sign in / Sign out and enable toggles. Sign-in runs `/mcp-auth` through Pi.
- **Session titles.** Auto-titling no longer picks uncallable catalog
  variants such as `gpt-5.4-mini-openai-compact`. Failures log the real API
  error; the last retry uses Pi's default model.
- **App icon.** Desktop, iOS, and landing use the new Cypher mark.
- **Linux CLI** is still the headless tarball (`cypher headless`, login,
  sync, daemon). macOS is the headed `.app` / `.dmg`.

## Tags & artifacts

Release tag: `cypher-v0.2.0` (immutable) — artifacts:
`cypher-0.2.0-linux-{x86_64,aarch64}.tar.gz`, `cypher-0.2.0-macos-arm64.dmg`,
`cypher-0.2.0-macos-arm64-app.tar.gz`.
