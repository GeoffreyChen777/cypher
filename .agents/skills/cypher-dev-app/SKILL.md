---
name: cypher-dev-app
description: Start, rebuild, restart and verify the local Cypher desktop development app (headed UI attached to a headless dev engine) without touching the installed app or other instances.
---

# Cypher development app

Use this skill to start, restart or smoke-test the local desktop dev app from
this repository. It follows `AGENTS.md`: after each verified feature, rebuild
and restart the dev UI, keep the running engine, then verify and report.

## Scope and ground rules

- Only touch dev processes and data under `~/.cypher-development/`. Never
  stop, restart or read the data of the installed app (`~/.cypher`, the
  `/Applications` bundle, its launchd daemon) or another checkout's instance.
- By default restart **only the dev UI**. The headless engine holds chats,
  runs and config; restart it only when the change needs it (engine, harness,
  sync or RPC code), and first check for running chats and tell the user.
- Never put Edge credentials in the UI environment (`dev-app.sh` scrubs them).

## Layout

- Binary: `target/debug/cypher` (`apps/cypher`); `cypher headless` is the engine.
- `scripts/dev-engine.sh [local|dev]` builds
  (`cargo build --locked -p cypher --features development`) and execs the
  engine with `CYPHER_HARNESS=mock` (override by exporting `CYPHER_HARNESS`),
  `CYPHER_AUTO_UPDATE=0` and data in `~/.cypher-development/<mode>-engine`.
- `scripts/dev-app.sh [local|dev]` execs the UI with data in
  `~/.cypher-development/<mode>-ui`, attached to that engine over private Unix
  IPC. It does not build, and refuses to start unless the engine is listening.
- `CYPHER_DEV_INSTANCE=<name>` (both scripts) suffixes both directories
  (`local-engine-<name>`, `local-ui-<name>`) so a second checkout never shares
  an engine with the first.
- Logs: each process writes `<data-dir>/logs/cypher-headless.log` or
  `cypher-headed.log` (previous launch kept as `.log.old`).

## Start

`local` mode is fully offline (`CYPHER_PROFILE=local`). From the repository
root, in two terminals or as background jobs:

```sh
mkdir -p ~/.cypher-development
nohup scripts/dev-engine.sh local > ~/.cypher-development/local-engine.log 2>&1 &
# wait for the engine (the first build takes a few minutes):
until CYPHER_DATA_DIR=~/.cypher-development/local-engine target/debug/cypher status --verbose \
  | grep -q 'IPC:      listening'; do sleep 1; done
nohup scripts/dev-app.sh local > ~/.cypher-development/local-ui.log 2>&1 &
```

`dev` mode syncs through a development Edge: run `cd apps/edge && npm run dev`
(a local `wrangler dev` on port 27640, bearer `dev-user@dev-org`), then
`scripts/dev-engine.sh dev` and `scripts/dev-app.sh dev`. The engine sources
the private env file at `CYPHER_DEV_ENV_FILE` (default
`~/Documents/cypher-development.env`); `CYPHER_DEV_EDGE_URL` names a staging
Edge instead. See `docs/local-edge.md`.
iOS: `scripts/dev-ios.sh` (see `apps/ios/README.md`).

## Rebuild and restart the UI

1. Identify the dev UI of this checkout and instance — never a process you
   cannot tie to it:

   ```sh
   ps -axo pid,ppid,lstart,command | grep '[t]arget/debug/cypher'
   lsof -a -p <pid> -d cwd -Fn          # cwd must be this repository
   ps eww -p <pid> | tr ' ' '\n' | grep '^CYPHER_DATA_DIR='   # must be …/local-ui[-<name>]
   CYPHER_DATA_DIR=~/.cypher-development/local-engine target/debug/cypher status --verbose
   ```

   The UI is the `target/debug/cypher` process without `headless`.
2. Build: `cargo build --locked -p cypher --features development`. If it fails,
   stop here and keep the old UI running.
3. Stop only that UI PID: `kill <pid>`; confirm it exited.
4. Relaunch: `nohup scripts/dev-app.sh local > ~/.cypher-development/local-ui.log 2>&1 &`.
5. Verify and report:
   - a new UI PID with this repository as cwd and the old PID gone;
   - the engine PID unchanged and `status --verbose` still shows `IPC: listening`;
   - this launch's `~/.cypher-development/local-ui/logs/cypher-headed.log` shows
     `engine daemon detected; connecting` and no new errors.

To restart the engine as well (after checking for running chats), stop the
`cypher headless` PID of this instance, start `scripts/dev-engine.sh` again,
wait for `IPC: listening`, then restart the UI as above.

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
- Engine already running / socket in use: run `cypher status --verbose` with
  the intended `CYPHER_DATA_DIR`. Socket paths derive from the UID and the
  canonical engine data directory; `CYPHER_IPC_PORT` has been removed. For a
  second instance use `CYPHER_DEV_INSTANCE`, never another port, and never
  unlink a socket you did not create. See `docs/unix-ipc.md`.
- `dev-app.sh` exits with "start dev-engine.sh first": the engine for that
  mode/instance is not listening; check its log.
- `dev-engine.sh dev` exits with "Missing …cypher-development.env": set
  `CYPHER_DEV_ENV_FILE` or create the private file; never commit it.
