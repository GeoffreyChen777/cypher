# Guided Linux setup

This flow requires **Cypher 0.3.3 or newer**. The deployment gate prevents
the new installer from going live while the public client channel is still
0.3.2, which has no `setup` command.

## Normal path

```sh
curl -fsSL https://edge.letscypher.app/install.sh | sh
```

The installer verifies the immutable client archive and launches `cypher setup`
using `/dev/tty` rather than reading answers from the installation pipe.
In an interactive SSH shell:

1. Confirm account connection (or choose local-only).
2. Open the displayed URL on your laptop, authorize, and paste the browser code.
   Workspace selection is requested only when needed.
3. Setup installs Pi Runtime with progress, using the same verified Runtime
   manager as the desktop application.
4. Setup installs/starts the systemd user service and checks the engine's
   identity and readiness. Remote success also requires the synced workspace
   connection to be established.
5. Select this device in Cypher desktop and configure Providers/MCP there.

Typical completion:

```text
✓ Device connected
Device:  gpu-server
Runtime: Pi 0.85.1
Service: running · starts at boot

In Cypher desktop, select this device and configure Providers / MCP.
```

This means the device and Runtime are ready, not that an LLM provider is
authenticated. Provider credentials are not copied automatically, and existing
local chats are not uploaded by account connection.

## Small command surface

```sh
cypher                    # setup initially, status after successful setup
cypher setup              # continue or retry onboarding
cypher status             # concise engine / Runtime / connection status
cypher status --verbose   # data directory, account and IPC diagnostics
cypher logs               # recent engine logs
cypher logs --follow      # stream logs
cypher update             # existing managed binary updater
```

The low-level `headless`, `login`, `logout`, `sync` and `daemon` commands remain.
The guided flow handles stopping and restarting its own idle service around
authentication; users do not need to chain `daemon stop; login; daemon start`.
It will not silently switch an already connected device back to local-only.

## SSH, automation and non-systemd environments

- No terminal: the installer performs a binary-only installation and prints one
  next step: `~/.local/bin/cypher setup`. Use an interactive SSH session
  (`ssh -t` when running a remote command).
- `install.sh --no-setup` explicitly requests binary-only installation.
- `cypher setup --non-interactive` never prompts and requires an existing
  complete account. For a new local-only device, use
  `cypher setup --local --non-interactive`.
- `cypher setup --foreground` prepares Runtime and runs the engine in the
  current terminal, without claiming background persistence. It stops when
  that terminal closes. This is also offered when no user service bus exists.
- Setup attempts unprivileged lingering first, and asks before invoking
  `sudo loginctl enable-linger`. If declined/unavailable, completion explicitly
  says the service may stop after logout.
- Install as the user whose privileges the agents should have. The service
  runs as that user; sudo is used only for the optional lingering permission.
- Bash/zsh/POSIX profiles receive an idempotent source line for a static
  `~/.cypher/shell-env.sh` PATH fragment; fish uses its own `conf.d` fragment.
  Symlink-managed profiles and unrelated existing fragments are left alone.
  The parent shell's PATH cannot be changed by a child installer; the absolute
  command works immediately and the short command works in configured new
  terminals.

## Existing installations and safety

Local Engine IPC now uses private Unix sockets, not a shared fixed port.
Different OS users and different data directories get distinct endpoints;
non-default data directories also get distinct user-service names. See
[Unix IPC](unix-ipc.md). Old `CYPHER_IPC_PORT` settings must be removed.

- A setup coordinator lock prevents concurrent wizards. The engine's existing
  data lock protects authentication and offline Runtime writes.
- A listener must match the persisted device ID before setup can manage it.
  A running unmanaged engine, a different unit/executable/data directory, or an
  incompatible saved service environment is not silently taken over.
- Busy engines are not stopped. An already-ready service using the current
  binary is left running; re-running setup does not restart it unnecessarily.
- Existing service configuration is preserved, not regenerated. Advanced
  `CYPHER_*` overrides and `~/.cypher/env` must match the caller's configuration.
  EnvironmentFile content is never executed as shell code.
- If setup pauses a service and authentication or Runtime installation fails,
  it attempts to start the previous service again. Failures are reported rather
  than hidden. Retry `cypher setup`; completed downloads and credentials survive.
- Ctrl-C, termination and SSH hangup cancel setup through the same recovery
  path. Authentication reader tasks are drained before releasing the data lock;
  a leftover blocking terminal read cannot keep the CLI process alive.
- Service processes and setup metadata are not considered ready merely because
  a command exited successfully: IPC identity/readiness and remote connection
  are checked. A pending connection is reported as incomplete, not success.
- Setup does not automatically install system packages, run arbitrary repair
  commands as root, or overwrite immutable release directories.

## Validation

`scripts/test-linux-cli.py` covers archive integrity, compatibility gating,
PATH idempotence and a real PTY/pipe handoff to setup. With a Linux binary,
additional tests use an isolated HOME, a loopback Runtime fixture, and test
service managers to cover local setup, idempotence, download failure, service
recovery and non-interactive/no-user-bus behavior. These are not a substitute for
testing an actual systemd user session and real browser authorization on Linux.
