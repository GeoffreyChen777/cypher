# Cypher

Control your coding agents (Claude Code, Codex, Cursor, Grok, Hermes, Pi) locally by default, with optional multi-device sync.

![Cypher driving a Claude Code session with a live branch diff sidebar](apps/landing/public/assets/app-screenshot.jpg)

Every device runs a small engine that stores sessions on that device. The engine
remains local-only unless you explicitly connect an account.

## Set up a Linux device

Guided setup requires the next client release, **0.3.3 or newer**.

```bash
curl -fsSL https://edge.letscypher.app/install.sh | sh
```

The installer opens one setup wizard: connect your account, install Pi Runtime,
start a **systemd user service**, and verify the device connection. Choose
local-only mode to skip account connection. If persistent startup needs
administrator permission, setup asks before invoking sudo.
Without a usable user service, setup offers foreground operation and clearly
states that it stops when the terminal closes.

In a non-interactive shell, only the binary is installed; run
`~/.local/bin/cypher setup` later in an SSH terminal. No login prompt hangs on
the installation pipe. See [Linux setup](docs/linux-setup.md).

Official Linux binaries support glibc-based x86_64 and aarch64 systems with
glibc 2.31 or newer (for example Ubuntu 20.04 or Debian 11 and newer).
Alpine/musl is not currently an official target.
This is a headless engine, not a terminal chat UI or Linux desktop application.

Day-to-day:

```bash
cypher             # setup on first use; concise status afterwards
cypher setup       # continue or repair setup
cypher status      # concise device status
cypher logs        # recent engine logs; --follow streams them
cypher update      # update to the latest release
```

`cypher --version` prints the installed binary's version. `cypher update --check`
exits 1 when an update is available; download/network errors also fail, so check
the diagnostic output. `cypher status --verbose` includes the original account,
data-directory and IPC diagnostics. Status does not refresh credentials.
Advanced `cypher daemon start|stop|restart|status` commands remain available.

### Pi and configuration

System Pi and `~/.pi` are not used. Pi Runtime (Node, Pi and curated plugins) is a
separate download, with per-device configuration under
`$CYPHER_DATA_DIR/pi-runtime/agent` (default `~/.cypher/pi-runtime/agent`).
The Linux setup wizard installs Runtime automatically. Sign the desktop into
the same account, select the Linux device in Settings, and configure Providers
and MCP there. Account login is not provider login.

The low-level `cypher headless` command remains non-interactive and does not
perform first-use setup. Dedicated terminal-only Provider/MCP management
subcommands are not available yet.

Use the same `CYPHER_DATA_DIR` for CLI commands and the service. Local IPC uses
a private per-user, per-data-directory Unix socket; there is no port to choose.
`CYPHER_IPC_PORT` is no longer supported. `cypher daemon install` captures data-directory and other supported `CYPHER_*`
overrides, resolving a relative data directory to an absolute path.
The optional `~/.cypher/env` is a **systemd EnvironmentFile**, not a shell script;
it overrides captured service values and is **not** automatically loaded by
one-shot CLI commands. Set matching shell variables when using it, especially
for account/data-directory settings. Do not put provider API keys there.

When a development UI uses a separate preferences directory, set
`CYPHER_ENGINE_DATA_DIR` to the headless engine's data directory. This setting
is UI-only; CLI commands use `CYPHER_DATA_DIR`. See [Unix IPC](docs/unix-ipc.md)
for ownership, permissions and multi-instance service behavior.

### Installation integrity and release compatibility

The online installer requires an adjacent `<artifact>.sha256` containing the
64-character digest. It verifies downloads, validates archive members, probes
the executable and only then replaces the `current` link. Missing/mismatched
checksums stop installation; existing versions and user data remain unchanged.
An existing conflicting version directory is not overwritten automatically.
The new installer must be deployed after a release containing both these
checksum files and the guided `setup` command (>= 0.3.3). Older incompatible
downloads are refused. SHA-256 is an integrity check, **not** a release signature.

The tarball's own `install.sh` is a manual, unmanaged installation to
`~/.local/bin`; use the online installer for managed self-updates.

## Optional multi-device sync

On Linux, run the setup wizard when you want to connect to desktop. It handles
safe service coordination around sign-in:

```bash
cypher setup
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

Unified and side-by-side Git comparison: [Git diff layouts](docs/git-diff.md).

Licensed under the [MIT License](LICENSE).
