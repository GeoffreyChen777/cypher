# Cloudflare development Edge

Endpoint: **https://cypher-edge-development.geoffreychen777.workers.dev**

This is an isolated dataset in the **same Cloudflare account** as production.
It does not isolate the account's quotas or bill. Do not run unrestricted load
tests here; use local workerd for exhaustion/failure tests.

## Resource and access boundaries

- Worker: `cypher-edge-development`, config `edge/wrangler.development.jsonc`.
- Separate DO namespaces, including a development-only `DevelopmentGuard`.
- R2: `cypher-development-blobs` and `cypher-development-releases` (no production bindings).
- No custom domain, preview URLs, production service bindings, WorkOS secret,
  APNs private key or notification delivery. Auth, install and release endpoints
  are disabled. Production migrations/configuration are untouched.
- `AUTH_MODE=dev-locked`: a random secret `DEV_ACCESS_TOKEN` authenticates the
  fixed identity `dev-user` / `dev-org`. Arbitrary dev bearers fail closed.
- Automatic invocation logs and traces are off to avoid storing WS token URLs.
- The private local API credential file is `~/Documents/cypher-development.env`
  (0600). Never commit it, echo its values, or copy production sessions into dev.

## Initial safety limits

The global gate includes HTTP room operations, WebSocket messages and alarms:

- 120 admitted operations/minute, 1,000/day (UTC reset).
- Four concurrent operations, with two-minute crash-recovery leases and
  conservative SQL headroom reservations. Neither protects against every
  possible oversized write inside an already admitted operation.
- Tripwire at 10,000 observed SQL rows written/day, including the gate's own
  accounting writes. Observed via native SQL cursors, not estimated from HTTP counts.
- Eight room identities for the lifetime of this deployment's guard data;
  midnight does not permit creating another eight objects. R2 blob access uses
  one shared slot. Reuse small fixtures rather than creating arbitrary chats.
- 64 KiB inbound HTTP bodies / WS frames; larger histories/checkpoints are
  intentionally unsupported in this initial development environment.
- Exhaustion returns HTTP 429 or WS close 1013; rejection does not write the
  gate's counter. Runtime-answered ping/pong does not run a room handler.

The SQL threshold is a **soft circuit breaker**, not an exact billing quota:
in-flight operations may overshoot, abrupt runtime termination can prevent
settlement, and this does not meter all Cloudflare storage/CPU categories or
production writes. The gate adds requests and writes of its own, so subtract
its overhead when evaluating the future streaming protocol. Cloudflare analytics
remain the source for billed usage. Do not claim this guarantees staying below
the account's daily free limit.

## Deploy and smoke test

Use only the guarded manual entrypoint, with the existing deployment credential
in the environment (not command-line arguments):

```sh
bash edge/scripts/deploy-development.sh
```

It validates resource isolation, typechecks and runs both Edge test tiers before
deploying. It does not enable the disabled production Actions workflow.

```sh
# Small remote smoke: health, denied auth, disabled endpoints, 2 WS peers,
# one opaque row, ACK/dedup, ping/probe, registry pull and accounting.
set -a
source ~/Documents/cypher-development.env
set +a
node edge/scripts/development-smoke.mjs
```

`GET /dev/budget` with the development bearer reports the persisted gate state.
The smoke script refuses production URLs; `DEV_SMOKE_EXHAUST=1` is allowed only
against localhost and verifies concurrent HTTP/WS rate-limit enforcement.

## Native development profiles

Official builds retain Production defaults (WorkOS, production Edge). Cloud
development requires the Rust `development` feature / iOS `CypherDev` scheme;
it is independent of compiler optimization. No credential is compiled in.

```sh
# Default offline development, separate data from the installed application:
bash scripts/dev-engine.sh local
bash scripts/dev-app.sh local       # second terminal

# Cloud development (reads the 0600 credential file described above):
bash scripts/dev-engine.sh dev
bash scripts/dev-app.sh dev         # second terminal
```

Cloud engine/UI data live under `~/.cypher-development/dev-engine` and
`dev-ui`. The UI attaches by IPC without receiving the secret. Engine auth
returns `dev-user` separately from its secret bearer, and neither Debug output
nor profile paths include the token. Never set the legacy `CYPHER_EDGE_TOKEN`
for this profile. The CLI rejects production data roots in cloud dev mode.
Headless builds can also opt in with `--no-default-features --features development`.
The shared dev token authorizes the development engine's remote RPC surface;
treat it as a remote-access credential. Separate data roots are not an OS
filesystem sandbox. Use mock harness/tiny disposable workspaces for protocol
tests, and never distribute this token to untrusted testers.

For iOS, select **CypherDev** in Xcode or run:

```sh
CYPHER_DEV_SIMULATOR=<dedicated-simulator-uuid> bash scripts/dev-ios.sh
```

The Dev bundle (`ai.mvp-lab.cypher.ios.dev`) has its own app sandbox,
Keychain service and `cypher-dev` callback. Its sign-in screen accepts a dev
token, not a WorkOS login. The simulator script passes the token through the
process environment; the app saves it in its own device-only Keychain for
subsequent launches. Signing out removes it. APNs is not bound in Dev.
For a physical phone, run the Dev scheme with a suitable development signing
profile, then enter the token in the secure field (not launch arguments).

`Cypher` Release/TestFlight retains the production bundle/callback and excludes
cloud development authentication and the opt-in interop runner. macOS/Linux
packaging probes the resulting executable to reject a dev-enabled artifact.
Before exporting an iOS distribution archive also run:

```sh
python3 scripts/ci/check-production-profile.py /path/to/Cypher.app
```

No formal application data or existing local development instance is reset.
These initial quotas support brief cross-device tests, not all-day cloud use;
use Local by default and stop cloud development processes when done.

### Cross-client interop probe

`CypherDev -dev-interop` operates only on the seeded `development-interop`
chat. It waits for the desktop transcript, sends a unique iOS Run through the
real command ledger and waits for a second assistant reply. The result is in
the Dev sandbox's `Documents/development-interop.json`. This runner is absent
from production builds. Test with mock harness first; it establishes transport
and command execution, not real-provider or physical-device acceptance.

## Validation, 2026-09-14

- Development deployment: `23bc7b33-1bf1-46ee-b9d2-5be27051882a`.
- Typecheck, 100 unit tests and 35 workerd tests passed.
- Real local workerd smoke verified simultaneous HTTP admission limits and
  budget enforcement on an already-connected WebSocket.
- Cloud smoke verified authentication, two-peer Chat2 relay, ACK/dedup,
  ping/probe, registry reading and SQL accounting, using only tiny fixtures.
- Cloudflare settings verified six development namespace IDs distinct from
  production and only the development R2 buckets/secret bound.
- Production remains at `b529c3a0-834f-426a-8922-26626ce16e2d`, 100% traffic.

### Native client integration

- Rust desktop/engine/sync/update regression run: 1,087 passed, 5 external
  tests ignored. Separate process-level profile tests: 4 passed in each of
  development-enabled and production builds. CI/release helper tests: 28 passed.
- iOS `CypherDev` (optimized Development) and `Cypher` (Release) simulator
  suites: 171 passed each. Production artifact checks confirm the official
  bundle/callback and absence of the development interop runner.
- Real cloud interop succeeded twice: desktop-origin transcript imported by
  the iOS Dev App; iOS-origin nonce Run executed by the mock harness in the
  native engine; assistant response returned to iOS. Native IPC confirmed the
  same nonce and completed replies in the desktop transcript.
- Second pass restarted/reinstalled iOS Dev without injecting a token again,
  verifying its isolated Keychain restoration. DeviceRoom `ListModels` RPC
  also completed through the cloud Edge.
- Headless WSS exposed a TLS provider ambiguity when built with UI-linked
  dependencies. The executable now installs the existing ring provider before
  either headless or UI startup; the verified engine established its relay
  without that panic.
- Native development uses a new data root; the previously running local
  engine and formal application were not restarted or reconfigured.
- These checks used the iOS simulator and mock harness, not a physical phone
  or billable provider run. No production deploy or TestFlight upload is part
  of this integration.
