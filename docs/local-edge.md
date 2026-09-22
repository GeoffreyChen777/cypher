# Local Edge development

The hosted development Worker (`cypher-edge-development`) was **retired on
2026-09-22**, together with its six Durable Object namespaces and the
`cypher-development-blobs` / `cypher-development-releases` buckets. It is gone,
not paused. Three reasons:

- it drew on the **same account allowance** as production (~33k Durable Object
  requests in the 10 days before removal);
- its `DevelopmentGuard` spent requests and rows of its own on every operation,
  so it distorted exactly the billing numbers it was used to measure;
- its room allowlist was capped at 16 for the lifetime of the guard data and had
  filled permanently, so it could no longer accept a new chat.

Local `wrangler dev` replaces it for every purpose, and is strictly better for
measurement: no guard, no quota, no cost, and exact per-invocation telemetry.

## Running an Edge locally

```sh
cd edge && npm run dev            # wrangler dev on 127.0.0.1:27640, AUTH_MODE=dev
```

`AUTH_MODE=dev` accepts `bearer == userId`, and only a `user@org` bearer carries
an org claim. That matters: `/registry/:orgId/*` and `/workspace/:orgId/*`
compare the claim against the URL and answer 403 without it, so a bare token —
including the private 64-hex development secret, which the retired dev-locked
Worker accepted on its own — cannot reach a registry route locally. Room
ownership is still claim-on-first-join per user, exactly as in production.

## Pointing a client at it

A development-profile engine defaults to `http://127.0.0.1:27640` — the port
above — so no configuration is needed for the common case:

```sh
scripts/dev-engine.sh dev
```

When that resolved endpoint is loopback, the script sends `dev-user@dev-org`
rather than the private secret: it is the `user@org` form `AUTH_MODE=dev`
needs, and it is the very identity the dev-locked Worker used to return, so it
maps to the `orgs/dev-org/dev-user` data directory that already exists. The
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
build, `CYPHER_PROFILE=development`, a 64-hex `CYPHER_DEV_ACCESS_TOKEN`, and a
data directory under `~/.cypher-development/`. Production builds and the
production profile cannot reach any of it.

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

Measured on production. Two regimes dominate, and they look completely
different, so a short sample will mislead you:

- **Active**: `PUT /chat2/{id}/tail` dominated at 62.5% of DO-bound HTTP. The
  tail rode the 1s snapshot-quiesce tick, so a streaming chat published once a
  second. It now has a 10s floor (`TAIL_MIN_PUBLISH_MS`, `doc_host.rs`), with
  the publish after the final change exempt so a settled chat is always exact.
  Measured on a real streaming run: 9 publishes where the old code would have
  sent ~171.
- **Idle**: `POST /notifications/activity` dominated at 75%. Both clients
  heartbeat every 15s. A repeat only refreshes state the Worker reads while the
  viewport is foreground **and** on a chat — `active()` requires foreground,
  `iosViewingChat` requires a matching chatId, and `target:{chatId}` is written
  only when both hold. Repeats outside that are now suppressed on desktop
  (`notification_activity.rs`) and iOS (`NotificationController.swift`), which
  removes background and list-view heartbeats outright.

Notification *events* are not a target: `notification_events.rs` dedupes on a
signature of status/startedAt/children, so only real transitions send. A
150-second production sample contained none.

## Where the requests actually went

A `wrangler tail` event carries an `entrypoint` field naming the Durable Object
class, which is the only way to attribute traffic — the `durableObjectId` alone
tells you nothing. Classifying a 305s production sample that way:

| source | rate | billable/h | share |
| --- | --- | --- | --- |
| `POST /notifications/activity` (RegistryRoom) | 378/h | 378 | 56.3% |
| RegistryRoom inbound websocket | 1830/h | 91.5 | 13.6% |
| DeviceRoom inbound websocket | 1133/h | 56.7 | 8.4% |
| `GET /device/{id}/status` (DeviceRoom) | 71/h | 71 | 10.6% |
| ChatRoom inbound websocket | 94/h | 4.7 | 0.7% |
| everything else (rows, ws, nudge, event, tail, checkpoint) | 70/h | 70 | 10.4% |
| **total** | | **672** | |

Two facts shape every decision here:

1. **Inbound websocket messages bill 20:1, HTTP bills 1:1.** Moving a message
   from HTTP onto a socket that is already open is a 20x saving, and moving it
   onto a frame that is *already being sent* is free.
2. **`ping` costs nothing.** All four rooms call `setWebSocketAutoResponse`, so
   the runtime answers keepalives without waking the object. Verified directly:
   20 pings produced 0 spans. Keepalives are never the problem; do not go
   looking for them.

### The activity heartbeat now rides the presence beat

The viewport reports which chat is on screen so the edge can suppress a push
for a chat you are already looking at. That report is a 15s heartbeat, and over
HTTP it was the single largest line on the bill.

A presence beat already goes to the *same* object on the *same* cadence over an
open socket. The refresh now rides it (`viewport_activity.rs`, the `activity`
field on the presence frame, `Notifications.applyActivity`). Transitions still
spend an HTTP request, because only they need the reply carrying `readEventIds`
and the badge.

Measured with `scripts/activity-transport-bench.mjs` against a local edge:
**1.050 → 0.050 billable requests per beat, a 21x reduction on that path.**
`edge/test/workerd/activity-presence.workerd.test.ts` proves the two transports
store identical state, so the saving is not bought with behaviour.

### A successful status probe now backs off

`relay_probe_task` re-verifies a device whose presence has gone stale. A probe
that answered `hostConnected=true` used to clear the backoff entirely and grant
only `PRESENCE_FRESH_MS` (45s) of freshness — against a 30s sweep. A device
that was alive but whose presence beat never arrived was therefore re-probed
about once a minute, forever.

Successful answers now back off too (`RelayProbeRetry::alive`, capped at 300s).
The subtlety worth keeping: a probe refreshes `presence_seen` itself, so the
candidate filter would read back its own stamp, mistake it for a heartbeat and
drop the backoff it had just set. `verified_at` distinguishes the two. A
genuine beat still clears it instantly, so a returning device never waits.

### Presence no longer republishes everything

Every inbound presence beat called `publish()`, which `send_replace`d four
watch channels whether or not anything had changed. Each wake-up becomes a
relay frame to every subscribed viewport — this is what the DeviceRoom's
1,133 messages/hour were: peers being re-sent data they already had.
`publish_if_changed` still writes the value through (a late subscriber must not
start stale) but wakes nobody when the snapshot is identical.

### What is left, and why it is hard

After the three changes above, roughly 191 billable requests/hour remain, and
**about half of that is the presence beat itself**: 1,830 messages/hour across
the registry rooms. The signature is unmistakable — no gap between messages
ever exceeds 15.1s, which is what a periodic beat looks like and what bursty
row pushes never do. Dividing by the 240/h a single client sends implies about
7.6 beating connections.

Cutting that further means slowing the beat, which directly lengthens how long
a device keeps showing "online" after it disappears. That is a user-visible
trade, not a free win, so it is a decision rather than a fix. For reference: a
30s beat halves the cost and doubles worst-case offline detection from 45s to
90s.

Worth checking before paying that price: every signed-in development engine
beats presence into the same room as the real client. Some of those 7.6
connections are likely development instances, not users.

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

- `docs/rows-written-baseline.md` — the deterministic fixture for rows written,
  replayed against real workerd + real Durable Object SQLite.
- `scripts/e2e-smoke.sh` — two headless engines and a local Edge, proving the
  cross-device command path end to end with the mock harness.
- `MIGRATION.md` §12.3 / §12.3b — the incident-hardened behaviours and the
  in-memory rebuild rule that **no** optimization may break, whichever backend
  it targets.
