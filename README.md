# Cypher

Control your coding agents (Claude Code, Codex, Cursor, Grok, Hermes, Pi) locally by default, with optional multi-device sync.

![Cypher driving a Claude Code session with a live branch diff sidebar](apps/landing/public/assets/app-screenshot.jpg)

Every device runs a small engine that stores sessions on that device. A new installation starts in local-only mode without an account or a network connection.

## Install and run locally (Linux)

```bash
curl -fsSL https://edge.letscypher.app/install.sh | sh
cypher status
```

The installer starts a **systemd user service** when a working user bus is
available. Otherwise run `cypher headless` under your own process supervisor.
For operation after logout and at boot, enable lingering:
`loginctl enable-linger "$USER"` (administrator permission may be required).
No Cypher account is required to start the local-only engine.

Official Linux binaries support glibc-based x86_64 and aarch64 systems with
glibc 2.31 or newer (for example Ubuntu 20.04 or Debian 11 and newer).
Alpine/musl is not currently an official target.
This is a headless engine, not a terminal chat UI or Linux desktop application.

Day-to-day:

```bash
cypher status      # local/synced mode and engine status
cypher update      # update to the latest release
cypher daemon start|stop|restart|status
journalctl --user -u cypher.service -f
```

`cypher --version` prints the installed binary's version. `cypher update --check`
exits 1 when an update is available; download/network errors also fail, so check
the diagnostic output. `cypher status` inspects saved account state without
refreshing credentials; it is not an online credential-validity test.

### Pi and configuration

System Pi and `~/.pi` are not used. Pi Runtime (Node, Pi and curated plugins) is a
separate download, with per-device configuration under
`$CYPHER_DATA_DIR/pi-runtime/agent` (default `~/.cypher/pi-runtime/agent`).
For desktop-to-Linux setup, sign both devices into the same Cypher account,
select Linux in the desktop Settings sidebar, install its Runtime under Agents,
then configure Providers and MCP there. Account login is not provider login.

`cypher headless` does not install the first Runtime automatically. Dedicated
terminal-only Runtime/Provider/MCP management subcommands are not available yet.

Use the same `CYPHER_DATA_DIR` and `CYPHER_IPC_PORT` for CLI commands and the
service. `cypher daemon install` captures these and other supported `CYPHER_*`
overrides, resolving a relative data directory to an absolute path.
The optional `~/.cypher/env` is a **systemd EnvironmentFile**, not a shell script;
it overrides captured service values and is **not** automatically loaded by
one-shot CLI commands. Set matching shell variables when using it, especially
for account/data-directory settings. Do not put provider API keys there.

### Installation integrity and release compatibility

The online installer requires an adjacent `<artifact>.sha256` containing the
64-character digest. It verifies downloads, validates archive members, probes
the executable and only then replaces the `current` link. Missing/mismatched
checksums stop installation; existing versions and user data remain unchanged.
An existing conflicting version directory is not overwritten automatically.
The new installer must be deployed with a release containing these checksum
files; it deliberately does not silently accept older checksum-less
downloads. SHA-256 is an integrity check, **not** a release signature.

The tarball's own `install.sh` is a manual, unmanaged installation to
`~/.local/bin`; use the online installer for managed self-updates.

## Optional multi-device sync

Sign in only when you want to open your account's synced workspace. Authentication changes the profile selected by the next engine start, so stop the daemon before changing it:

```bash
cypher daemon stop
cypher login
cypher daemon start
```

You can then start an agent on one synced device and follow or drive it from another. An always-on machine such as a VPS can keep those agents working after you close your laptop.

Signing in does not upload, move, or import existing local sessions. Local sessions and their attachments remain under the local profile and reappear when you return to local-only mode:

```bash
cypher daemon stop
cypher logout
cypher daemon start
```

`cypher login` and `cypher logout` refuse to modify credentials while an engine owns the data directory. The desktop app follows the same next-restart profile boundary.

On macOS: use the desktop release, or build `cypher` from source and run `cypher daemon install` to install the launchd service.

---

Developing or curious how it works? [![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/zeronsh/comet) or check out [ARCHITECTURE.md](ARCHITECTURE.md).

CI, deployment prerequisites and release recovery: [CI/CD operations](docs/ci-cd.md).

Chat fonts, colors, spacing and wide-screen mode: [Chat appearance](docs/chat-appearance.md).

Overall themes and Terminal, Git and Sidebar color overrides: [Appearance colors](docs/appearance-colors.md).

Licensed under the [MIT License](LICENSE).
