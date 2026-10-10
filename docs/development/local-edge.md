# Local Edge development

There is no hosted development Worker; the former `cypher-edge-development`
deployment, its Durable Object namespaces and buckets were deleted. Local
`wrangler dev` replaces it for every purpose, and is strictly better for
measurement: no guard, no quota, no cost, and exact per-invocation telemetry
([Cloudflare billing and measurement](../operations/cloudflare-billing.md)).

## Running an Edge locally

```sh
cd apps/edge && npm run dev            # wrangler dev on 127.0.0.1:27640, AUTH_MODE=dev
```

`AUTH_MODE=dev` accepts `bearer == userId`, and only a `user@org` bearer carries
an org claim. That matters: `/registry/:orgId/*` compares the claim against the
URL and answers 403 without it, so a bare token — including the private 64-hex
development secret — cannot reach a registry route locally. Room
ownership is still claim-on-first-join per user, exactly as in production.

## Pointing a client at it

A development-profile engine defaults to `http://127.0.0.1:27640` — the port
above — so no configuration is needed for the common case:

```sh
scripts/dev-engine.sh dev     # builds and execs the development engine
scripts/dev-app.sh dev        # in another terminal: a UI attached to it over IPC
```

`dev-app.sh` refuses to start unless the matching `dev-engine.sh` engine is
already listening, so it never embeds a second engine.
`CYPHER_DEV_INSTANCE=<name>` on both scripts keeps a second checkout's engine
and UI data in their own `~/.cypher-development/<mode>-engine-<name>` and
`<mode>-ui-<name>` directories.

`dev-engine.sh dev` and `dev-ios.sh` read the private development env file
(`CYPHER_DEV_ACCESS_TOKEN`, optionally `CYPHER_DEV_EDGE_URL`) from
`~/Documents/cypher-development.env`, or from the path in `CYPHER_DEV_ENV_FILE`;
they stop with an error naming the path when it is missing.

When that resolved endpoint is loopback, the script sends `dev-user@dev-org`
rather than the private secret: it is the `user@org` form `AUTH_MODE=dev`
needs, and it maps to the existing `orgs/dev-org/dev-user` data directory. The
private secret is still what goes to a remote staging endpoint.

The credential guard (`apps/cypher/src/main.rs`,
`development_credential_is_valid`) follows the same split. A remote development
Edge takes that deployment's 64-hex shared secret and nothing else. A loopback
Edge additionally accepts a `user@org` identity — the 64-hex shape cannot carry
an org claim, and on loopback there is no shared secret to protect. Exactly one
`@` is required, so the identity a local Edge derives is unambiguous.

`CYPHER_DEV_EDGE_URL` overrides the endpoint for a self-hosted staging server:

```sh
CYPHER_DEV_EDGE_URL=https://edge-dev.example.com scripts/dev-engine.sh dev
```

`cypher status --verbose` prints the resolved `Edge:` line — check it before
concluding which deployment a run exercised.

The development bearer is a shared secret, so the override is fenced
(`apps/cypher/src/main.rs`, `development_edge_is_safe`):

- the production Edge is rejected outright, trailing slash and case included;
- `http://` is accepted only for `localhost`, `127.0.0.1` and `[::1]`;
- anything without an `https://` (or loopback `http://`) scheme is rejected.

The rest of the development contract is unchanged: a `development` feature
build, `CYPHER_PROFILE=development`, a `CYPHER_DEV_ACCESS_TOKEN` bearer (against a
loopback Edge in `AUTH_MODE=dev` this is the identity `dev-user@dev-org`, which
`scripts/dev-engine.sh` sets), and a data directory under `~/.cypher-development/`.
Production builds and the production profile cannot reach any of it.

The iOS Dev bundle (`CypherDev` scheme, `scripts/dev-ios.sh`) follows the same
rules (`apps/ios/Cypher/App/DevelopmentProfile.swift`): it defaults to
`http://127.0.0.1:27640`, which the simulator reaches on the Mac's loopback, and
connects there as `dev-user@dev-org` with no secret. `CYPHER_DEV_EDGE_URL` names a
staging Edge instead, which takes the 64-hex `CYPHER_DEV_ACCESS_TOKEN`;
`dev-ios.sh` passes both through `SIMCTL_CHILD_*` and drops a stale value naming
the retired Worker. Debug builds can still point at any loopback Edge with
`-setedge <url> -setmode dev`.

## Related

- [Cloudflare billing and measurement](../operations/cloudflare-billing.md) —
  measuring what a change costs locally, watching the production bill, and
  release-artifact retention.
- `apps/edge/test/workerd/rows-optimization.workerd.test.ts` — the deferred-write
  optimizations pinned against real workerd + real Durable Object SQLite.
- `scripts/e2e-smoke.sh` — two headless engines and a local Edge, proving the
  cross-device command path end to end with the mock harness.
