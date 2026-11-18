---
name: cypher-dev-app
description: Start, rebuild, restart, screenshot and stop the local Cypher desktop development app (headed UI attached to a headless dev engine) without touching the installed app or other instances.
---

# Cypher development app

Use this skill to start, restart, screenshot or smoke-test the local desktop
dev app from this repository. It follows `AGENTS.md`: after each verified
feature, rebuild and restart the dev UI, keep the running engine, then verify
and report. Repository layout, checks and conventions:
`docs/development/README.md`; the development Edge: `docs/development/local-edge.md`.

## Scope and ground rules

- Only touch dev processes and data under `~/.cypher-development/`. Never
  stop, restart or read the data of the installed app (`~/.cypher`, the
  `/Applications` bundle, its launchd daemon) or another checkout's or
  session's instance: use and stop only the instance named by your own
  `CYPHER_DEV_INSTANCE`, through its own pid files.
- Agents always use their own instance: `CYPHER_DEV_INSTANCE=<name>` with a
  name for this worktree or task (e.g. its branch, `smarter-judge`), never the
  unnamed default, which belongs to the main checkout.
- The engine runs the mock harness (`CYPHER_HARNESS=mock`, the default of
  `dev-engine.sh`); export another `CYPHER_HARNESS` only when the task needs it.
- By default restart **only the dev UI**. The headless engine holds chats,
  runs and config; restart it only when the change needs it (engine, harness,
  sync or RPC code), and first check for running chats and tell the user.
- Never put Edge credentials in the UI environment (`dev-app.sh` scrubs them).

## Layout

- Binary: `target/debug/cypher` (`apps/cypher`); `cypher headless` is the engine.
- `scripts/dev-engine.sh [local|dev]` builds
  (`cargo build --locked -p cypher --features development`, or the features in
  `CYPHER_DEV_FEATURES`) and execs the engine with `CYPHER_HARNESS=mock`,
  `CYPHER_AUTO_UPDATE=0` and data in `~/.cypher-development/<mode>-engine`.
- `scripts/dev-app.sh [local|dev]` execs the UI with data in
  `~/.cypher-development/<mode>-ui`, attached to that engine over private Unix
  IPC. It does not build, and refuses to start unless the engine is listening.
- `CYPHER_DEV_INSTANCE=<name>` (all dev scripts) suffixes both directories
  (`local-engine-<name>`, `local-ui-<name>`).
- Both scripts write their pid (kept by `exec`) to `<data-dir>/dev.pid`.
- `scripts/dev-instance.sh` drives one instance end to end (below). It builds
  with `development,dev-capture`, so the UI runs the dev capture driver
  (`crates/ui/src/shell/dev_capture.rs`) with
  `CYPHER_CAPTURE_DIR=<ui-dir>/capture`.
- Logs, all inside the instance's data dirs:
  - the app's own: `<data-dir>/logs/cypher-headless.log` and
    `cypher-headed.log` (previous launch kept as `.log.old`);
  - the scripts' stdout/stderr (build output, early panics):
    `<engine-dir>/logs/dev-engine.out`, `<ui-dir>/logs/dev-app.out`.

## Agent workflow: start, check, screenshot, stop

Run each line as written, from the repository root, as its own command. They
are plain commands on purpose: the permission classifier reads them as local
work (no danger, nothing "known only when it runs"), so the reviewing model can
approve work on the chat's own dev instance, and an approval can be kept. All
of them run outside the sandbox (they write `~/.cypher-development/`, open a
window and reach the engine's Unix socket): escalate them, labelled with the
instance. An approval kept for `scripts/dev-instance.sh` covers the script's
current contents only; after editing the script the next call asks again.

```sh
CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh start
CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh status
CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh capture main
CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh stop all
```

- `start` builds (the first build takes several minutes), starts the engine
  unless its `dev.pid` names a live engine of this checkout, waits for
  `IPC: listening`, then starts the UI the same way and waits until its
  capture driver listens. A new UI data dir is seeded with
  `{"setupCompleted":true,"piRuntimeSetupVersion":1}` so it opens on the main
  window; delete `<ui-dir>/ui-settings.json` (UI stopped) to see onboarding.
  A fresh instance still shows "What's new" first: `drive "key escape"`
  closes it.
- `status` prints both pids, the engine's IPC state and the log and capture
  paths.
- `capture NAME` writes `capture NAME` to `<ui-dir>/capture/cmd`, waits for
  the driver's `ok done` in `<ui-dir>/capture/log` and prints the PNG path,
  e.g. `~/.cypher-development/local-ui-<name>/capture/main.png`. Open it
  with the file read tool. The image comes from gpui's own renderer
  (`Window::render_to_image`), so it needs no Screen Recording permission and
  works while the window is covered.
- `drive LINE…` sends several driver lines in one go, for interactions before
  a capture: `"size 1200 800" "click 40 60" "wait 500" "key cmd-k" "type hi"
  "capture palette"`. All commands: the header of
  `crates/ui/src/shell/dev_capture.rs` (`offscreen` then `size W H` yields
  Retina captures on a 1x display).
- `stop` stops the UI; `stop all` the UI, then the engine. Each reads
  `<data-dir>/dev.pid`, checks that the pid is this checkout's
  `target/debug/cypher` with this repository as cwd and that data dir as
  `CYPHER_DATA_DIR`, sends SIGTERM and waits until it exited. A pid that fails
  the check is left alone (and the command fails).
- `restart-ui` builds, stops this instance's UI and starts it again; the engine
  keeps running.

`CYPHER_DEV_MODE=dev` selects `dev` mode (below) for the same commands.

### Commands that ask the owner

The first two rows are dangers to the classifier and always ask the owner
(a `kill` only when the process is not known to be this chat's). The
subshell/redirection row leaves words or paths known only when the line runs,
so no approval can be kept and the reviewing model judges (and may ask) every
single time. The reviewer never approves other instances' data, and `/tmp`
logs get lost or mixed up between sessions. Use the alternative instead:

| Avoid | Use |
|---|---|
| `osascript`, System Events, AppleScript-driven `screencapture` | `dev-instance.sh capture` / `drive` |
| `kill <pid>` of a pid found with `ps`/`pgrep` | `dev-instance.sh stop` (pid file in the instance's own dir) |
| logs in `/tmp`, beside the data dir, or in another instance's dir | the instance's own `logs/` (the script does it) |
| `( … )` subshells, `case`, `> "$(…)"` and other computed redirections, `source`/`.` of an env file, `mktemp` | one plain command per line with literal paths; let the scripts do the rest |
| writing or reading another instance's data (`local-ui-<other>`, the unnamed default) | your own `CYPHER_DEV_INSTANCE` only |

## Manual start (two terminals)

`local` mode is fully offline (`CYPHER_PROFILE=local`). For a person at the
keyboard; agents use `dev-instance.sh`:

```sh
scripts/dev-engine.sh local    # terminal 1; waits in the foreground
scripts/dev-app.sh local       # terminal 2, once the engine is up (it exits if not)
```

`dev` mode syncs through a development Edge: run `cd apps/edge && npm run dev`
(a local `wrangler dev` on port 27640, bearer `dev-user@dev-org`), then
`scripts/dev-engine.sh dev` and `scripts/dev-app.sh dev`. The engine script
reads the private env file at `CYPHER_DEV_ENV_FILE` (default
`~/Documents/cypher-development.env`); `CYPHER_DEV_EDGE_URL` names a staging
Edge instead. See `docs/development/local-edge.md`.
iOS: `scripts/dev-ios.sh` (see `apps/ios/README.md`).

## Rebuild and restart the UI

1. `CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh restart-ui`. It builds
   first; if the build fails it stops there and the old UI keeps running.
2. Verify and report:
   - `CYPHER_DEV_INSTANCE=<name> scripts/dev-instance.sh status`: a new UI pid,
     the engine pid unchanged and `engine IPC: listening`;
   - this launch's `~/.cypher-development/local-ui-<name>/logs/cypher-headed.log`
     shows `engine daemon detected; connecting` and no new errors (read it
     with the file tools or `grep` on that literal path).

To restart the engine as well (after checking for running chats):
`stop all`, then `start`.

## Demo data

`scripts/dev-demo.sh [--work] [--slow]` builds, starts an offline mock engine,
seeds demo projects and chats, and opens the UI. Data lives in
`/tmp/cypher-demo-*` (`--work`: `/tmp/cypher-demo-work-*`); delete it to
reseed. `--work` seeds a run whose thoughts and commands fold into one row;
`--slow` paces mock replies to watch streaming. Quitting the app stops the
engine.

## Troubleshooting

- `cargo: command not found`: the toolchain is pinned by `rust-toolchain.toml`;
  use the rustup proxy (`rustup which cargo` shows the resolved binary) and do
  not prepend a fixed toolchain directory to `PATH`.
- `start` fails with "engine exited": read
  `~/.cypher-development/local-engine-<name>/logs/dev-engine.out` (build errors,
  "another cypher engine is already running").
- Engine already running / socket in use: run
  `CYPHER_DATA_DIR=~/.cypher-development/local-engine-<name> target/debug/cypher status --verbose`.
  Socket paths derive from the UID and the canonical engine data directory;
  `CYPHER_IPC_PORT` has been removed. For a second instance use
  `CYPHER_DEV_INSTANCE`, never another port, and never unlink a socket you did
  not create. See `docs/design/unix-ipc.md`.
- "pid … is not this checkout's UI": the pid file is stale and the pid was
  reused, or another checkout runs on this data dir. Pick another instance
  name; do not kill the process.
- `capture` times out: the UI binary was built without `dev-capture` (started
  by hand or by another script); `restart-ui` rebuilds and restarts it.
- `dev-app.sh` exits with "start dev-engine.sh first": the engine for that
  mode/instance is not listening; check its log.
- `dev-engine.sh dev` exits with "Missing …cypher-development.env": set
  `CYPHER_DEV_ENV_FILE` or create the private file; never commit it.
