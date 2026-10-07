# Local Edge development

There is no hosted development Worker; the former `cypher-edge-development`
deployment, its Durable Object namespaces and buckets were deleted. Local
`wrangler dev` replaces it for every purpose, and is strictly better for
measurement: no guard, no quota, no cost, and exact per-invocation telemetry.

## Running an Edge locally

```sh
cd edge && npm run dev            # wrangler dev on 127.0.0.1:27640, AUTH_MODE=dev
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

iOS switches endpoints separately, through the `-setedge <url>` launch argument
(`apps/ios/Cypher/App/AppModel.swift`); `scripts/dev-ios.sh` injects only the
access token, via `SIMCTL_CHILD_CYPHER_DEV_ACCESS_TOKEN`.

## Measuring what a change costs

`wrangler dev` records every invocation as a queryable span, including the ones
that decide the Durable Object bill and are invisible to a SQL-level probe.
`scripts/edge-billing-local.mjs` turns those spans into a billing summary:

```sh
cd edge && npm run dev &                                   # leave running
MARK=$(node scripts/edge-billing-local.mjs --mark)         # scope the run
# ...drive traffic: scripts/e2e-smoke.sh, a dev engine, curl, anything...
node scripts/edge-billing-local.mjs --since "$MARK"
```

It reports billable Durable Object requests split into HTTP (1:1), WebSocket
events (20:1) and alarm calls, requests grouped by in-room path, and exact rows
written per query — with miniflare's internal bookkeeping table excluded,
because the real runtime does not write it.

Verified faithful on one point that matters: a runtime-answered `ping` produces
no span, matching production, where `setWebSocketAutoResponse` keeps the
keepalive from waking the room.

## Watching the bill

`scripts/cf-usage.py` reports every metered dimension against its Workers Paid
inclusion for the **current billing cycle**, read from the subscription rather
than assumed to be a calendar month:

```sh
CLOUDFLARE_API_TOKEN=... python3 scripts/cf-usage.py            # table
CLOUDFLARE_API_TOKEN=... python3 scripts/cf-usage.py --json     # for a cron job
```

It exits non-zero when any meter is projected at or above `--fail-at` (default
80%), so it works as a scheduled guard. The token needs Account Analytics Read,
plus Billing Read for exact cycle boundaries.

It reports the **billed** Durable Object request count, not the dashboard's.
The dashboard counts every inbound WebSocket message as an invocation; billing
folds them 20:1, so the dashboard overstates the bill by a large factor:

```
billable = http + alarm + hibernation / 20
```

Two measurement caveats it surfaces rather than hides. R2 storage analytics lag
about a day, so the figure is labelled with the date it came from — for a
just-changed bucket, list the objects instead. And storage is a level, not an
accumulation, so it is never extrapolated to the end of the cycle.

## What the meters cost, and why

Production traffic has two regimes, so a short sample misleads: while chats
stream, chat2 traffic dominates; while idle, the per-device presence and
notification-activity beats do. Rules that keep both cheap:

1. **Inbound WebSocket messages bill 20:1, HTTP bills 1:1.** Moving a message
   from HTTP onto an open socket is a 20x saving; riding a frame that is
   already being sent is free.
2. **`ping` costs nothing.** All four rooms call `setWebSocketAutoResponse`, so
   the runtime answers keepalives without waking the object (20 pings produced
   0 spans). Keepalives are never the problem.
3. **Hosts do not publish the chat2 tail.** Nothing reads it; the Edge still
   serves `GET`/`PUT /chat2/{id}/tail` so older hosts keep working.
4. **Viewport activity rides the presence beat.** The 15s "which chat is on
   screen" refresh travels in the presence frame (`viewport_activity.rs`,
   `Notifications.applyActivity`); only transitions spend an HTTP
   `POST /notifications/activity`, because only they need the reply carrying
   `readEventIds` and the badge. Repeats outside a foreground chat are
   suppressed on desktop (`notification_activity.rs`) and iOS
   (`NotificationController.swift`). `activity-presence.workerd.test.ts`
   proves both transports store identical state.
5. **Notification events send only on real transitions**:
   `notification_events.rs` dedupes on a signature of
   status/startedAt/children.
6. **Status probes back off.** `relay_probe_task` re-verifies a device whose
   presence went stale. A successful answer backs off too
   (`RelayProbeRetry::alive`, capped at 300s); `verified_at` keeps the probe's
   own `presence_seen` stamp from being mistaken for a heartbeat. 404 (never
   hosted) and 403 (not your room) are authoritative "not live" and walk the
   offline backoff to its 1800s cap; network errors, 5xx, 429 and 401 stay
   inconclusive. A genuine presence beat clears the backoff instantly.
7. **Presence republishes only changes.** `publish_if_changed` writes the value
   through for late subscribers but wakes nobody when the snapshot is
   identical.

What remains is mostly the presence beat itself (a 15s beat per connection).
Slowing it lengthens how long a vanished device keeps showing "online" (a 30s
beat halves the cost and doubles worst-case offline detection from 45s to 90s),
so that is a product decision, not a fix. Signed-in development engines beat
into the same room as the real client, so count them before paying that price.

To attribute production traffic, `scripts/edge-billing-prod.py` samples
`wrangler tail` and classifies each event by its `entrypoint` (the Durable
Object class — the `durableObjectId` alone tells you nothing). It counts only
the Durable Object event of each call, not the Worker-level one, and cannot tell
*which* device a request targets because tail redacts id path segments.

## Release artifacts

`cypher-releases` grew ~84 MB per application release and ~640 MB per Runtime
revision, forever, and had reached 86% of the R2 free tier. `release.py` now
applies retention after a successful publish (`KEEP_APP_VERSIONS`,
`KEEP_RUNTIME_VERSIONS`), and `prune-releases` runs it by hand:

```sh
CLOUDFLARE_ACCOUNT_ID=... CLOUDFLARE_API_TOKEN=... \
  python3 scripts/ci/release.py prune-releases            # dry run
  python3 scripts/ci/release.py prune-releases --apply
```

Safety is structural, not conventional: a key is a candidate only if it parses
as a versioned artifact or manifest, falls outside the keep window, and is named
by no live pointer. Pointers are never candidates, so an unexpected key is
always kept. Retention on the publish path is non-fatal — the release has
already promoted, and housekeeping must not fail it.

## Related

- `edge/test/workerd/rows-optimization.workerd.test.ts` — the deferred-write
  optimizations pinned against real workerd + real Durable Object SQLite.
- `scripts/e2e-smoke.sh` — two headless engines and a local Edge, proving the
  cross-device command path end to end with the mock harness.
- [`docs/plans/MIGRATION.md`](plans/MIGRATION.md) §12.3 / §12.3b — the incident-hardened behaviours and the
  in-memory rebuild rule that **no** optimization may break, whichever backend
  it targets.
