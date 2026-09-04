---
name: cypher-dev-app
description: Build and launch the Cypher desktop development app locally, including its headless engine, with macOS toolchain and shell compatibility troubleshooting.
---

# Cypher development app

Use this skill when asked to start, run, or smoke-test the local Cypher desktop app in this repository.

## Repository layout

- Repository root: the directory containing `Cargo.toml`
- Desktop binary: `apps/cypher`
- Binary name: `cypher`
- Demo helper: `scripts/dev-demo.sh`
- Edge worker: `edge/` (not required for the local desktop app)

## Preferred launch flow

Run from the repository root:

```sh
cd /path/to/cypher

# The execution environment may expose rustup but not cargo.
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"

cargo build -p cypher -q
```

Start a local engine on a dedicated IPC port:

```sh
mkdir -p /tmp/cypher-dev-daemon /tmp/cypher-dev-ui

CYPHER_DATA_DIR=/tmp/cypher-dev-daemon \
CYPHER_IPC_PORT=27921 \
CYPHER_HARNESS=mock \
RUST_LOG=warn \
./target/debug/cypher headless > /tmp/cypher-dev-engine.log 2>&1 &
ENGINE_PID=$!
```

Wait until the engine is listening before starting the UI. On macOS, a simple
probe is:

```sh
for _ in $(seq 1 40); do
  (exec 3<>/dev/tcp/127.0.0.1/27921) 2>/dev/null && {
    exec 3>&-
    break
  }
  sleep 0.25
done
```

Launch the headed app:

```sh
CYPHER_DATA_DIR=/tmp/cypher-dev-ui \
CYPHER_IPC_PORT=27921 \
RUST_LOG=warn \
nohup ./target/debug/cypher > /tmp/cypher-dev-ui.log 2>&1 &
UI_PID=$!
```

Verify both processes:

```sh
ps -axo pid,ppid,stat,command | grep -E 'target/debug/cypher' | grep -v grep
```

The UI is successfully launched when a `./target/debug/cypher` process is
present alongside `./target/debug/cypher headless`. The app uses the local
workspace under `/tmp/cypher-dev-ui`; it does not need the edge worker or
authentication.

## Demo data

`scripts/dev-demo.sh` builds the binary, starts a mock engine, seeds demo
spaces/chats, and opens the UI. It is useful when a populated visual demo is
needed:

```sh
./scripts/dev-demo.sh
# or:
./scripts/dev-demo.sh --slow
```

The script currently relies on Bash associative arrays (`declare -A`). The
system `/bin/bash` on older macOS installations is Bash 3 and fails with:

```text
declare: -A: invalid option
```

Do not silently substitute `zsh`: although it accepts some of the syntax, its
associative-array behavior can produce invalid space IDs during seeding. If a
compatible Bash 4+ is available, invoke the script with that interpreter:

```sh
/opt/homebrew/bin/bash ./scripts/dev-demo.sh
```

If no compatible Bash is installed, use the preferred launch flow above for a
working empty development workspace, or install/use Bash 4+ before running
the demo seeder.

## Troubleshooting

### `cargo: command not found`

The environment may have rustup at `/opt/homebrew/bin/rustup` but no
`$HOME/.cargo/bin/cargo` shim. Resolve cargo with:

```sh
rustup which cargo
```

Then prepend the containing toolchain `bin` directory to `PATH`, for example:

```sh
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
```

### Port `27921` is already in use

Find and stop the stale local engine, or choose another unused port and use it
consistently for both `CYPHER_IPC_PORT` values:

```sh
ps -axo pid,command | grep -E 'target/debug/cypher headless' | grep -v grep
kill <engine-pid>
```

### UI opens without seeded chats

The UI can run with an empty workspace. This is expected if the demo script
failed during its shell-dependent seeding phase. Check:

```sh
tail -n 80 /tmp/cypher-dev-engine.log
tail -n 80 /tmp/cypher-dev-ui.log
```

### Stop the development app

Keep the engine alive while using the UI. When finished:

```sh
kill "$UI_PID" "$ENGINE_PID" 2>/dev/null || true
```

If the PID variables are no longer available, locate the processes with
`ps` and terminate the matching `target/debug/cypher` and
`target/debug/cypher headless` processes.
