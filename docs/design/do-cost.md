# Durable Object cost

Where the Edge's Durable Object (DO) requests come from, and how to cut them without
changing what the app does. Measurement tools and the earlier cost rules are in
[Cloudflare billing and measurement](../operations/cloudflare-billing.md).

Status: analysis and design only. Nothing below is built, deployed or measured live. Source
for every number: one usage snapshot, 2026-10-24..2026-11-23, and the code at `53c34485`.

## Summary

- **Billed is not the dashboard number.** The dashboard's 3,616,140 "requests" count every
  inbound WebSocket message; billing folds those 20:1. Billed: **about 0.99M in the window,
  1.09M a month at the current run rate**, against 1M included.
- **The money is small**: $0.15 per million over. Today's saving is at most the $0.15 on
  the invoice; at 10× the devices, $1.48 a month before and $0.54 after. The case for the
  work is headroom, a flatter cost per device and a bound on what one bad release can cost.
  At 100× the DO meter that costs money is rows written (about $204 a month), not requests
  ($16).
- **65% of the bill is fixed-interval polling that runs whether or not anything changed**:
  trust chain every 60 s (41%), bot inbox every 60 s (10%), presence beat (8%), relay
  status probes (5%). Every one of those clients already holds an open socket to the Edge.
- **Four changes take billed from 1.09M to about 0.46M a month (−58%)**:
  [T1](#t1-trust-chain-head-over-the-registry-socket) trust chain head pushed over the
  registry socket (−414k), [B1](#b1-bot-inbox-acknowledged-nudges) bot inbox on
  acknowledged nudges (−103k), [C1](#c1-hedged-https-pull-on-connect) HTTPS pull only when
  the socket is late (−70k), [D1](#d1-peer-dial-cooldown-that-follows-presence) dial
  cooldown for unreachable peers (−41k). A fifth,
  [N1](#n1-settle-chats-the-edge-called-hidden) (0–40k, unmeasured), and the storm guards
  [G1–G3](#g1-to-g3-guards-against-request-storms) ride the same release.
- **The dashboard number moves less: 3.79M to about 3.10M (−18%)**, because three quarters
  of it are WebSocket messages. Only [P1](#p1-presence-one-beat-per-room) and
  [S1](#s1-session-freshness-from-host-presence) move it further (to about 2.0M), for 5% of
  the bill; they are optional.
- **No Worker-only change saves anything large.** The Edge parts deploy first and change
  nothing until clients use them; the saving needs one engine/desktop release and one iOS
  build, and arrives per updated device.
- **Two billing rules are open, and one invoice line settles both**: whether overage is
  charged per whole million or pro rata, and whether auto-answered text `ping` frames bill
  at 20:1. If they do, pings are the largest line (about 1.0M a month) and
  [K1](#k1-protocol-level-keepalive-contingent) goes first.
- What to decide: [Decisions for the owner](#decisions-for-the-owner).

## Recommended plan

### Phase 0: measure before any code

None of these needs a code change; each needs a Cloudflare token or the invoice, so none
was run here.

| # | Measurement | How | Settles |
|---|---|---|---|
| M1 | The invoice line for Durable Objects requests: billed quantity and amount, and its cycle dates | Dashboard, Billing, the invoice and "Billable usage"; compare with `meters[key=do_requests].used` and `detail.do_invocations_raw` from `CLOUDFLARE_API_TOKEN=... python3 scripts/ops/cf-usage.py --json` run for the same cycle | Quantity about 1.0–1.2M and one whole unit charged: overage is per whole million, pings are not billed, the model here holds. Quantity about 2.0M: overage is pro rata and about 1.0M is unexplained by `http + alarm + hibernation/20`, which is what auto-answered pings at 20:1 would add; K1 becomes the first change. No DO request line at all: the $0.15 is another product and today's DO saving is $0 |
| M2 | HTTP / alarm / WebSocket split per class and day | The query below (the query at `scripts/ops/cf-usage.py:103` with `date` added). `cf-usage.py` itself has no per-day or per-namespace option: it sums one billing cycle for the account | Replaces the modelled split in [How the classes were split](#how-the-classes-were-split); gives the baseline every option is measured against |
| M3 | RegistryRoom, ChatRoom and DeviceRoom HTTP by path | `python3 scripts/ops/edge-billing-prod.py --seconds 305 --keep /tmp/tail.json` on a working day, twice | The guessed lines: `/notifications/event` (N1, N2), `/rows` and `/checkpoint` beside sockets (C1), `/device/*/status` (D2) |
| M4 | Who dials the DeviceRoom since Nov 21 | M2's query with `dimensions{date objectId type}` and the DeviceRoom namespace filter; on the engines, `grep -c "peer: dial attempt failed"` and `"peer: dialing via device room"` in the engine log | Whether the Nov 21–23 churn is one client dialing one unreachable host (D1) and which caller keeps asking |
| M5 | Trust polls per user after T1 ships | M2's query with `dimensions{date objectId}` and the TrustRoom namespace filter | Which users still run an engine that polls every 60 s |

```graphql
query($a:String!,$s:Date!,$e:Date!){viewer{accounts(filter:{accountTag:$a}){
  g: durableObjectsInvocationsAdaptiveGroups(limit:5000,
       filter:{date_geq:$s,date_leq:$e}){          # one class: add namespaceId:"<id>"
    sum{requests} dimensions{date namespaceId type} # type = http | alarm | hibernation
}}}}
```

Billed per class and day = `http + alarm + hibernation / 20`. Namespace ids: RegistryRoom
`94b9ece650784a6082d7e6daa438b84b`, ChatRoom `a7f5993fa4f043dcaa39af0bfca6fb02`, DeviceRoom
`cb92d0b5167b47b29635a2c6e200df6e`, TrustRoom `35e11edafd074276a00c01345ae7ce03`, BotRoom
`79fea7aba6ba42ebb45fd52cca35095b`, PushDevice `ac4e947b24634487893dbf8c3676d753`.

### Ranked options

Savings are per 30 days at the Nov 17–23 run rate (36,310 billed and 126,171 analytics
requests a day), with every client updated. Ranked by billed saving over effort, risk as
the tie-breaker. "Confidence" is that of the line the option removes.

| Rank | Option | Class | Saved billed / month | Saved analytics / month | Confidence | Risk to the experience | Effort | Ships with |
|---:|---|---|---:|---:|---|---|---|---|
| 1 | [T1](#t1-trust-chain-head-over-the-registry-socket) chain head over the registry socket, 15 min safety poll | TrustRoom | 414k (354k at 5 min, 429k at 30 min) | 414k | measured | low: a removal arrives in about 1 s instead of up to 60 s; up to 15 min only if the push and its retry both fail | M | Worker, then engine, then iOS |
| 2 | [B1](#b1-bot-inbox-acknowledged-nudges) inbox pulled on acknowledged nudges, 15 min safety pull | BotRoom | 103k (134k at the Nov 23 level; 40k more per connected bot) | 103k | measured | low: a lost nudge is repeated after 15 s (today: next poll, up to 60 s) | S–M | Worker, then engine |
| 3 | [C1](#c1-hedged-https-pull-on-connect) HTTPS pull only when the socket has not joined in 1.5 s | RegistryRoom, ChatRoom | 70k (68k–87k) | 70k | derived | low: first rows of a cold open up to 0.1–0.2 s later on a healthy network, 1.5 s later where WebSockets are blocked | S | engine, iOS |
| 4 | [D1](#d1-peer-dial-cooldown-that-follows-presence) dial cooldown grows while the peer is known offline | DeviceRoom | 41k (0 when no caller keeps asking for a dead peer) | 98k | inferred | low–medium: a call to an offline device fails at once instead of after a dial; return is detected by presence | S | engine |
| 5 | [N1](#n1-settle-chats-the-edge-called-hidden) one report per hidden chat instead of three per transition | RegistryRoom | 0–40k | 0–40k | guessed | none: the Edge already says it will never notify | S | engine |
| 6 | [G1–G3](#g1-to-g3-guards-against-request-storms) healthy-session backoff, dial budget, miss-refresh backoff, Edge backoff header, daily usage guard | all | 0 at run rate; an incident like Nov 6–11 cost about 166k billed in six days | same | inferred | low | S–M | engine; ops |
| 7 | [N2](#n2-notification-reports-over-the-registry-socket) notification reports as socket frames | RegistryRoom | up to 55k | 0 | guessed | medium: new frame pair, reply needed | M–L | Worker, engine, iOS |
| 8 | [D2](#d2-relay-status-probe-cap-by-age) relay status probe cap grows with how long a device has been gone | DeviceRoom | up to 25k | up to 25k | derived | changes a fallback bound (30 min → up to 6 h) | S | engine |
| 9 | [P1](#p1-presence-one-beat-per-room) one presence beat per room, the rest read from ping timestamps | RegistryRoom | 42k | 837k | derived | medium: presence margins shrink | M | Worker, engine, iOS |
| 10 | [S1](#s1-session-freshness-from-host-presence) session freshness from host presence | RegistryRoom | about 11k | about 225k, and about 1M rows written | guessed | medium: every viewer must be updated first | M–L | engine, desktop, iOS |
| 11 | [PD1](#pd1-no-badge-only-push-after-an-alert) no badge-only push to a phone that just got the alert | PushDevice | up to 3k | up to 3k | guessed | none | S | Worker only |
| if M1 says so | [K1](#k1-protocol-level-keepalive-contingent) protocol-level pings instead of text `ping` | every socket | about 1.0M (0 if pings are not billed) | 0 | open | low–medium: host liveness in DeviceRoom reads text pings | S–M | engine, iOS |

### Phases

| Phase | What | Where it ships | Saving when done |
|---|---|---|---|
| 0 | M1–M4 (M5 after T1) | nothing ships | none; removes the two billing unknowns and the three guessed lines |
| 1 | Edge parts of T1 and B1, G3's header, PD1 | Worker (deploys on merge) | about 3k (PD1); the rest are enablers: old clients see no difference |
| 2 | T1, B1, C1, D1, N1, G1, G2 client parts in one engine/desktop release | desktop 0.4.x + headless engines (auto-update) | per updated engine; about 625k billed a month once all are updated |
| 3 | T1 and C1 on iOS | iOS build | the phones' share (a foregrounded phone polls like an engine; not measured separately) |
| 4 | optional, in this order when wanted: N2, D2, P1, S1 | as listed | up to 130k billed, about 1.1M analytics |

K1 moves into phase 2 if M1 shows pings are billed.

### Target

Per 30 days, every client updated, pings not billed:

| | Billed | Share of the included 1M | Analytics |
|---|---:|---:|---:|
| Today (run rate Nov 17–23) | 1,089k | 109% | 3,785k |
| After T1 + B1 | 573k | 57% | 3,269k |
| After phase 2: T1 + B1 + C1 + D1 (N1 not counted) | **462k** | **46%** | **3,101k** |
| After phase 4 as well | about 330k | 33% | about 2,000k |

Headroom after phase 2: 538k a month, which is three incident weeks of the Nov 6–11
size or 30 more always-on engines at the new per-engine cost.

If pings are billed (M1), add about 1.0M to every "Billed" row and K1 removes it again.

### At 10× devices

Billed requests per 30 days per unit:

| Term | Grows with | Before | After phase 2 | After phase 4 |
|---|---|---:|---:|---:|
| Trust chain | engine online time (fixed poll) | 43,200 per always-on engine | 2,880 + 1 per change | same |
| Presence beat | engine online time (fixed beat) | 8,640 per always-on engine | 8,640 | about 4,700 (per room, not per device) |
| Relay status probe | engines × stale device rows | about 5,700 per engine | same | about half |
| Bot inbox | connected bots (fixed poll) | 43,200 per bot | 2,880 + 2 per message | same |
| Registry or chat (re)connect | connects | 2 | 1 | 1 |
| Chat open | opens | 2.3 | 1.3 | 1.3 |
| Streaming | minutes streamed | up to 1.5 + 0.15 freshness | same | up to 1.5 |
| Notification report | status transitions | 1; 3 and more for a hidden chat | 1; once per hidden chat | 0.05 |
| Dial to an unreachable peer | callers × time | 3 per 64.5 s | 1 per 600 s | same |

| | Fixed polls and beats | Activity | Total | Fixed share |
|---|---:|---:|---:|---:|
| 1×, before | 710k | 379k | 1,089k | 65% |
| 1×, after phase 2 | 193k | 269k | 462k | 42% |
| 10×, before | 7.1M | 3.8M | 10.9M ($1.48 overage) | |
| 10×, after phase 2 | 1.9M | 2.7M | 4.6M ($0.54 overage) | |
| 100×, before / after | | | 109M ($16.19) / 46M ($6.78) | |

The other DO meters at scale, from the periodic statistics of the window: rows written are
2.54M of 50M included, so they cross the inclusion at about 20× and cost about $204 a month
at 100× ($1.00 per million); duration is 28,466 GB-s of 400,000, crossing at about 14× and
costing about $31 a month at 100×. The session freshness touch (S1) is the largest steady
row writer; DeviceRoom's duration (79% of the total, concentrated in the two incident
weeks) has no established cause.

### What not to do

| Looks attractive | Why not |
|---|---|
| Slow the presence beat to 30 s, or widen the 45 s freshness window | Offline detection goes from 45 s to 90 s (`docs/operations/cloudflare-billing.md` already calls it a product decision); saves 45k billed |
| Lengthen the trust or bot poll without a push | Revocation and bot-message latency grow with the interval |
| A Worker cache or KV in front of `/trust/chain`, `/bots/*/inbox` or `/device/*/status` | The Cache API is per data centre and a write in one cannot purge another: a removal, a pairing or a bot message would be late by up to the TTL for a device served elsewhere. KV needs a new binding and moves the cost to KV reads |
| Liveness swept by an alarm | An alarm bills 1, a WebSocket message 1/20: a 15 s alarm per room costs as much as 20 clients' beats |
| Debounce or coalesce registry and chat pushes further | They already bill 1/20 (about 33k billed in the window for all of them); chat pushes are already at most one per 2 s |
| Cut the bytes or count of outbound frames, or the presence rebroadcast | Outbound messages are free |
| Remove the HTTPS pull paths | They are the offline recovery and the only path where WebSockets are blocked; C1 keeps them and only stops the duplicate |
| Send `ping` less often | If pings are not billed it saves nothing; if they are, a longer interval lengthens half-open detection (45 s lease). K1 removes the cost without touching the interval |
| One multiplexed socket for all chats | A large protocol change to save upgrades that scale with use, not with time; an idle hibernated socket costs nothing |
| Chase the 3.6M figure | 2.77M of it are WebSocket messages worth 0.14M billed; P1 and S1 remove 1.06M of them for 53k billed |
| Drop the relay status probe | It is the only liveness path that shares nothing with the registry room |
| More Worker-level short-circuits | 401 is already answered before any object (`apps/edge/src/index.ts:50-51`), the registry's org check too (`apps/edge/src/room-routes.ts:72`), unknown paths end in the Worker (`apps/edge/src/room-routes.ts:150`); what reaches an object needs the object |
| RPC methods instead of `stub.fetch` | An RPC session bills as one request, the same |

## Option designs

Common ground for every design that adds something to a socket protocol:

| Fact | Where | Consequence |
|---|---|---|
| The Rust and iOS registry clients redial on a frame whose `t` they do not know | `crates/sync/src/registry/actor.rs:498-503`, `apps/ios/Cypher/Sync/RegistryClient.swift:244-249`, `:420-424` | A new server frame type must be sent only to sockets that asked for it in `hello` |
| Both clients ignore unknown fields in frames they know | `crates/sync/src/registry/frames.rs:33-68` (no `deny_unknown_fields`), `apps/ios/Cypher/Sync/RegistryClient.swift:388` (keyed decode) | New fields on `state` and `probe-ok` are safe for old clients |
| RegistryRoom reads only `cursor` and `device` from `hello` and answers an unknown client frame with `error`, which clients log | `apps/edge/src/registry/registry-room.ts:327-333`, `:312-313`, `crates/sync/src/registry/actor.rs:550-553` | A new field on `hello` is safe against an old Worker; a new client frame type is not sent until the room advertised it |
| A relay host ignores a frame kind it does not know | `crates/rpc/src/device_room/relay.rs:406-408` | New nudge kinds are safe for old engines |
| Rust wire structs of the bot network tolerate unknown fields | `crates/proto/src/bot_wire.rs:276-281` | The inbox page can carry a capability flag |
| The Worker deploys on merge; a room is reachable only through the Worker, which always stamps the user header | `apps/edge/src/router.ts:29-44`, `apps/edge/src/bots/bot-room.ts:341` | Room-to-room paths are closed to clients by refusing requests that carry the header |

### T1: trust chain head over the registry socket

Today every engine and every foregrounded phone reads the whole chain every 60 s
(`RESYNC` `crates/engine/src/device_trust.rs:41`, loop `:355-392`;
`apps/ios/Cypher/Sync/DeviceTrustStore.swift:262-270`). TrustRoom wrote 146 rows in the
window (chain entries and pairing mailboxes) and answered 352,030 requests.

| Part | Design | Code touched |
|---|---|---|
| TrustRoom | Keep a `subscribers` table of registry room names. After a successful append (`apps/edge/src/trust/trust-room.ts:141-143`) call each subscriber's `/trust-head` with `{length}` from `ctx.waitUntil`; on failure keep a pending flag and retry from an alarm (5 s, doubling to 5 min). Serve `GET /head?subscribe=<room>` (room-to-room only) with `{length}`. Needs `REGISTRY_ROOMS` from `env`, unused today (`:36`) | `apps/edge/src/trust/trust-room.ts` |
| Worker | Forward the org to the registry room so it can name itself: add `org=` to the `ws` route's query (`apps/edge/src/room-routes.ts:75`) | `apps/edge/src/room-routes.ts` |
| RegistryRoom | `hello` may carry `caps:["trust"]`, kept in the socket attachment (`apps/edge/src/registry/registry-room.ts:327-333`). `state` carries `trust:{length}` when the room knows it (`:342-349`). On the first capable `hello` without a known length, subscribe at the user's TrustRoom (one room-to-room request per registry, again after `/reset` `:254-261`) and then send `{t:"trust",length}`. `/trust-head` (before the user-header check `:173-174`, refused when the header is present) stores the length in `meta` and sends `{t:"trust",length}` to ready sockets with the capability | `apps/edge/src/registry/registry-room.ts` |
| Sync crate | `hello.caps`, `ServerFrame::Trust`, `State.trust`, a `RegistryEvent::Trust(length)` | `crates/sync/src/registry/frames.rs:11-68`, `crates/sync/src/registry.rs:84-93`, `crates/sync/src/registry/actor.rs:312-316`, `:505-554` |
| Engine | On `Trust(length)` different from the local chain length: `sync()` now. While the registry is joined to a room that sent `trust`: next background sync after `SAFETY` (15 min) instead of `RESYNC`. On registry disconnect: the next sync is due 60 s after the disconnect, cancelled by a rejoin whose `state.trust` matches. A room that never sends `trust` (old Worker) leaves the engine at 60 s | `crates/engine/src/device_trust.rs:355-392`, `crates/engine/src/host/workspace_host/sync.rs:108-129` |
| iOS | Same three rules | `apps/ios/Cypher/Sync/RegistryClient.swift:309-316`, `:380-425`, `apps/ios/Cypher/Sync/DeviceTrustStore.swift:262-270` |

The hint carries only a length and is never trusted: the client still fetches the chain
over HTTPS, verifies it and accepts only an extension of its own
(`crates/engine/src/device_trust.rs:505-527`). A wrong or missing hint can delay a sync,
never change its result; the Edge can already withhold entries from a poll.

| | |
|---|---|
| Saving | Before: 14,886 a day = 10.3 engine-equivalents × 1,440. After: 10.3 × 96 safety polls + about 100 a day for changes, engine starts and pairing = 1,092. Saved 13,794 a day, **414k a month**, billed and analytics alike (all HTTP). 5 min safety: 354k. 30 min: 429k. Added: one room-to-room request per registry per chain change (under 200 a month) |
| Latency | Removal, policy or role change: about 1 s (push) instead of 0–60 s. A device not joined to its registry: 60 s as today. Both the push and TrustRoom's retries lost, or a socket that delivers pongs but no frames until the 900 s quiet probe (`crates/sync/src/registry.rs:61`) catches it: up to the safety interval |
| Invariant to test | A device is never more than 60 s without either a joined registry session that carries `trust`, or a chain read |
| Tests | `apps/edge/test/workerd/trust-room.workerd.test.ts`: append reaches a capable socket and not a plain one; `state` carries the length; a failed notify is retried. `crates/engine/src/device_trust/tests.rs` with the in-memory edge: hint → one read within 1 s; joined and hinted → no read for 15 min; disconnect → read at 60 s; no `state.trust` → 60 s cadence. Registry mock: `crates/sync/src/registry/mock_server.rs` |
| Staged check | Deploy the Worker; run one development engine with the change beside a released one; remove a device from a third: the log line at `crates/engine/src/device_trust.rs:514` appears within 2 s on the new engine |
| Old clients | No `caps`: they never receive `trust`, ignore `state.trust`, keep polling. New client on an old Worker: `caps` ignored, no `state.trust`, keeps polling |
| Effort | M: two rooms, one route, sync crate, engine, iOS |
| Measure after | M2 for TrustRoom, `type=http`: 14.9k a day → about 1.1k; each engine still on an old version adds 1,344 (M5 shows whose) |

Built on branch `cypher/do-cost-cuts` (2026-11-25), as designed, with these choices:

- `state.trust` goes only to a `trust`-capable hello, like the frame (old clients would
  ignore the field, but nothing needs it).
- RegistryRoom keeps the largest length it was told; a late retry with an older length is
  dropped. TrustRoom pushes its current length on every attempt.
- Both room-to-room routes answer 404 when the user header is present.
- Engine: the workspace host reports its registry session through a trust hook
  (`WorkspaceHost::set_trust_hook`), replayed when the hook is set, after the first join
  and after a lagged event pump, so a missed event only leaves the 60 s cadence. A failed
  read keeps the existing 10 s → 300 s backoff.
- iOS: a failed read retries after 60 s even while heads are carried. A phone that is not
  in the group holds an empty chain, so any head reads at once (no more than today).
- Not done: the staged check (needs a Worker deploy).

### B1: bot inbox, acknowledged nudges

The room already nudges the host's DeviceRoom when an entry is queued
(`apps/edge/src/bots/bot-room.ts:297-314`, call sites `:693`, `:712`, `:721`, `:742`,
`:761`), and the DeviceRoom queues the nudge when the host is offline and replays it on the
next host join (`apps/edge/src/device/device-room.ts:244-287`). The minute poll
(`PULL_EVERY` `crates/engine/src/bots/network/host.rs:26`, `:48-53`, `:86-99`) exists because
the nudge is best effort (`apps/edge/src/bots/bot-room.ts:311-313`). Two holes make it so:
a nudge "delivered" to a host socket that died less than 75 s ago is lost
(`apps/edge/src/device/device-room.ts:48`, `:115-130`, `:246-250`), and a failed
room-to-room call is dropped. A third: progress states are not nudged at all
(`apps/edge/src/bots/bot-room.ts:764-776`), so today's "working" and "awaiting review"
marks ride the poll.

| Part | Design | Code touched |
|---|---|---|
| BotRoom | Read the nudge's answer (`delivered` or `queued`, `apps/edge/src/device/device-room.ts:249`, `:260`). `queued`: done, the DeviceRoom replays it. `delivered` or a failed call: if `pull()` (`apps/edge/src/bots/bot-room.ts:421`) is still non-empty 15 s later, nudge again from the alarm: 15 s, 30 s, 60 s, then every 60 s ten times, then every 15 min. `sweep` arms the alarm for the earlier of expiry and re-nudge (`:277-287`). Nudge on `state` too. The inbox page says `push: true` | `apps/edge/src/bots/bot-room.ts` |
| Engine | When the last page of a room said `push`, pull it when nudged, at engine start (`all`, `crates/engine/src/bots/network/host.rs:61`) and every 15 min; otherwise every 60 s as today. `ASK_POLL` (15 s while `ask_bot` waits, `crates/engine/src/bots/network.rs:51`) stays | `crates/engine/src/bots/network/host.rs:26`, `:86-99`, `crates/proto/src/bot_wire.rs:276-281` |

| | |
|---|---|
| Saving | Before: 3,800 a day in Nov 17–23 (2.6 connected bots × 1,440), 4,880 on Nov 23. After: 2.6 × 96 + about 100 a day for nudged pulls and re-nudges = 353. **103k a month** at the week's average, 134k at the Nov 23 level, 40k more for every further connected bot. Billed and analytics alike. Added: 2 requests per message (nudge + pull) and 2 per progress state, which the poll used to absorb |
| Latency | Question, answer, link change: unchanged (nudge, about 1 s). Lost nudge: repeated after 15 s instead of found by the next poll (up to 60 s). Progress states: about 1 s instead of up to 60 s |
| Tests | `apps/edge/test/workerd/bot-room.workerd.test.ts`: unacknowledged entry after `delivered` → alarm and second nudge at 15 s; `queued` → no alarm; `state` nudges; ack stops the ladder. Engine: a fake `BotEdge` counts pulls: none in 15 min without a nudge when `push` is set, one a minute when not |
| Staged check | Two development engines, one question each way; drop the host's relay socket without closing it and ask again: the question lands within 15 s of the rejoin |
| Old clients | Old engine: ignores `push`, polls every minute, acknowledges, so the ladder never runs. New engine on an old Worker: no `push`, polls every minute |
| Effort | S–M: one room, one engine file, one wire field |
| Measure after | M2 for BotRoom: `http` 3.8–4.9k a day → under 0.5k; `alarm` stays near 0 (a rising alarm count means nudges are being lost) |

Built on branch `cypher/do-cost-cuts` (2026-11-25), as designed, with these choices:

- Ladder: the first repeat 15 s after the nudge, then 30 s, then 60 s eleven times (the
  "60 s, then every 60 s ten times"), then every 15 min; it gives up after 109 repeats
  (about a day), so a room whose host never acknowledges (a bot moved off the device)
  stops costing alarms. The next entry starts it over. State is two `meta` keys
  (`renudge_at`, `renudge_step`).
- Outcomes: `{queued: true}` ends the ladder (DeviceRoom replay); `{delivered: true}`, a
  thrown call or a 5xx arms it; a 4xx (the DeviceRoom has no owner yet, or another
  owner) ends it, since repeating cannot help and the host pulls everything when it first
  joins.
- The ack that leaves `pull()` empty clears the ladder and re-arms the alarm, so a nudge
  that was acknowledged in time costs no alarm. The alarm is now set to the exact
  earliest deadline (expiry or repeat) and deleted when there is none.
- Progress: `/peer/state` nudges the asking host only when the state changed, which
  includes the `delivered` mark the answering host's ack sends. Same `bots` nudge kind,
  so old engines see nothing new.
- Engine: the push flag is per room, from its last page. The rejoin pull needs a new
  callback, `HostRelayConfig::with_on_join` (`crates/rpc/src/device_room/relay.rs`),
  fired on every host connect; the next pass then pulls every room, like engine start.
  The start pull now waits for a pass that has an Edge. The loop still wakes every 60 s
  for owner edits; only the pull is gated by the 15 min safety.
- Tests: six workerd cases drive a BotRoom with fake DeviceRoom answers and a mocked
  clock (`apps/edge/test/workerd/bot-room.workerd.test.ts`, "BotRoom nudge repeats").
  Engine cases use a scaled cadence (`Bots::set_pull_cadence`, 100 ms for the minute,
  1 h for the safety) and a fake Edge that can drop nudges and answer with or without
  `push` (`crates/engine/tests/it/bots/network.rs`, B1 section).
- No iOS code pulls the bot inbox.
- Not done: the staged check (needs a Worker deploy and two engines).

### C1: hedged HTTPS pull on connect

Every registry and chat connect starts an HTTPS pull at the same instant as the WebSocket
dial, and the socket's `hello` then returns the same rows: at start
(`crates/sync/src/registry/actor.rs:52`, `crates/sync/src/chat_client/actor.rs:68`) and
after every dropped session (`crates/sync/src/registry/actor.rs:140`,
`crates/sync/src/chat_client/actor.rs:162`). iOS does the same with a poll at `start`
(`apps/ios/Cypher/Sync/RoomSocketLifecycle.swift:106-120`).

Design: start the dial; start the pull only if the session has not joined after 1.5 s, or
at once when the dial fails or times out (unchanged: `crates/sync/src/registry/actor.rs:88`,
`:103`, `crates/sync/src/chat_client/actor.rs:108`, `:123`), or when a local write is
waiting to be sent (unchanged: `crates/sync/src/registry/actor.rs:70-71`). iOS: the first
poll wake after 1.5 s instead of 0, the 20 s cadence while not joined unchanged.

| | |
|---|---|
| Saving | Registry: 386 dropped sessions a day → 11.6k a month. Chat: 1,990 sessions a day ended by a drop, plus those that closed cleanly → 60k–80k a month. About 5% of connects still pull: **68k–87k a month, 70k used in the target**, billed and analytics alike. In an incident it halves the cost of every reconnect (49k of the window's 55k registry drops fell in the two incident weeks) |
| Confidence | Derived: one pull per connect is read from the code; the count of connects is the disconnect-status count, a lower bound. M3 measures `/rows` directly |
| Latency | Healthy network: rows come with the socket's `state` instead of with whichever of the two answered first; the pull can win today by a round trip or two because HTTPS reuses a connection and the socket opens a new one: up to 0.1–0.2 s later on a cold open, 0 when the local copy is current. WebSocket blocked or slow: 1.5 s later than today. Live edits, pushes and offline recovery: unchanged |
| Tests | `crates/sync/src/chat_client/tests`, `crates/sync/src/registry/mock_server.rs`: join inside 1.5 s → the transport mock saw no request; dial stalls → one pull at 1.5 s; dial fails → pull at once; pending local write → pull at once |
| Staged check | Time from open to first remote row on a cold chat in a development instance, 20 opens before and after, with the WebSocket allowed and blocked |
| Old clients | Client-only; the Edge is untouched |
| Effort | S: two actors and the iOS lifecycle |
| Measure after | M3: `/rows` and `/checkpoint` requests per hour on RegistryRoom and ChatRoom fall to about 5% of the upgrade count; M2: ChatRoom `http` per `clientDisconnected` session falls from about 2.3 to about 1.1 |

Built on branch `cypher/do-cost-cuts` (2026-11-25), with these choices:

- One small type, `crates/sync/src/hedge.rs` (`HEDGE_DELAY` 1.5 s), used by both actors.
  A connect after the client's start or after a joined session dropped arms the hedge; the
  next dial starts it; `state` cancels it. "Joined" is the `state` answer, so a chat
  backfill that is still running on a live socket does not also pull.
- Pull at once, unchanged or added: dial failure and timeout; a session that never got
  `state`; a local write waiting (registry `pending_len`, chat outbox) at start or at the
  drop; a nudge or a re-run request during the dial or backoff (each such pull stands in
  for the hedge). Added beyond the design: **a cold start** (registry doc with no cursor and
  generation 0, chat with cursor 0) pulls beside its first dial, so a never-synced copy shows
  its first rows as fast as before; and **a reconnect after a stalled socket** (probe
  deadline; for chat also push-ACK, row-repair and backfill deadlines and exhausted gap
  repairs) pulls at once, since that socket was open but not delivering. Only the
  start of a client that synced before and the reconnect after an ordinary drop hedge.
- iOS: `RoomSocketLifecycle.start` sends a `.startPoll` wake at once, which pulls only for a
  cold copy (registry `helloCursor() == nil`, chat `cursor() == 0`); the first `.poll`
  comes after `Timing.firstPollNs` (1.5 s), then every 20 s while not joined, as before.
  iOS has no reconnect pull to remove.
- Tests: registry against the mock (a TCP port that never answers the upgrade, a new
  `MockRegistryServer::set_state_delay`), `crates/sync/tests/registry_transport.rs`; chat
  against channel pipes with paused time, `crates/sync/src/chat_client/tests/recovery.rs`
  (C1 section); iOS `RegistryClientLifecycleTests` (hedged first poll).

Cold-open measurement (`crates/sync/examples/cold_open.rs`, 20 opens per row, before =
`5a9804e2`, after = this change). Method: the real `RegistryClient` and `ChatClient` against
in-process servers (`MockRegistryServer`, a two-row chat frame server) on this Mac. The
network is modelled: the socket goes through a proxy that holds a new connection for 2 RTT
(TCP and TLS) and delays each chunk by RTT/2 each way; the HTTPS pull answers after 1 RTT
(a warm connection). Time from opening the client to its first remote row, median in ms
(p90 within 2% except where noted):

| Copy | WebSocket | RTT | Registry before → after | Chat before → after |
|---|---|---|---|---|
| cold | works | 50 ms | 54 → 54 | 52 → 52 |
| cold | works | 150 ms | 154 → 154 | 152 → 152 |
| cold | stalled | 50 / 150 ms | 54 → 54 / 155 → 154 | 53 → 53 / 153 → 153 |
| warm (new rows since) | works | 0 ms | 3 → 11 | 2 → 12 |
| warm | works | 50 ms | 54 → 211 | 52 → 273 |
| warm | works | 150 ms | 154 → 613 | 152 → 772 |
| warm | stalled | 50 ms | 54 → 1,568 | 53 → 1,564 |
| warm | stalled | 150 ms | 154 → 1,656 | 154 → 1,657 |
| either | refused | 50 / 150 ms | unchanged (54, 154–159) | unchanged (52, 152–154) |

Reading: a cold open is unchanged in every case (the cold rule). A blocked or stalled socket
costs 1.5 s, as the design said. On a healthy network a warm copy now waits for the socket,
which in this model is 3 RTT slower than the warm pull for the registry (TCP, TLS, upgrade,
then `state`) and about 4.4 RTT for chat (plus the rows request): +0.16 s / +0.22 s at
50 ms, +0.46 s / +0.62 s at 150 ms. The design's +0.1–0.2 s assumed the pull wins by one or
two round trips; it holds here only up to about 50 ms RTT. The warm case only matters when
rows changed while the client was away: its local copy is on screen at once. If that delay
shows in use, the cheaper lever is a shorter hedge for warm starts, not dropping C1 (the
saving comes from reconnects, where nobody waits).

Not done: the staged check in a development instance (20 opens with the real Edge).

### D1: peer dial cooldown that follows presence

A call to another device dials its DeviceRoom up to three times, 1.5 s and 3 s apart, then
cools down 5 s doubling to 60 s (`crates/rpc/src/device_room/links.rs:30`, `:333-336`,
`:473-499`, `:558-574`). The room accepts a client even with no host
(`apps/edge/src/device/device-room.ts:170-171`) and bounces `host_offline` on its first
frame (`:300-306`). A caller that keeps asking for an unreachable device therefore costs 3
upgrades per 64.5 s = 4,019 a day. Nov 21–23 averaged 4,013 dropped DeviceRoom sessions a
day (2,450 / 6,350 / 3,240) with 2.4 requests each, where Nov 17–20 had 302: the signature
of one such caller, on average. M4 confirms or refutes it.

Design: when the relay itself answered `host_offline` and the peer's presence is stale,
the cooldown cap rises from 60 s to 600 s and the next sequence is one dial, not three.
Everything that says the peer may be back resets it at once, as today: its presence beat
(`crates/engine/src/host/workspace_host/presence.rs:207-212`,
`crates/engine/src/runtime.rs:496-497`, `crates/rpc/src/device_room/links.rs:518`), a relay
status probe that answers alive, a system wake. A network error (not an answer from the
relay) keeps today's 60 s cap. The first failure keeps its three attempts, which cover the
host's own rejoin window (`crates/rpc/src/device_room/relay.rs:176-178`).

| | |
|---|---|
| Saving | Per caller stuck on a dead peer: 4,019 → 144 upgrades a day (−96%) once the ladder is at its cap. On the week's churn (1,591 upgrades and 3,754 requests a day above the Nov 17–20 level), taking −87% to allow for the ladder's first steps: **about 41k billed, 98k analytics a month**; 0 if the churn has stopped by itself |
| Risk | A device that is back but whose presence does not reach this engine stays refused for up to 10 min instead of 1; the status probe (`crates/engine/src/host/workspace_host/presence.rs:18`) and a wake shorten that. Decide after M4 names the caller: if it is a background task, it should stop asking instead |
| Tests | `crates/rpc/src/device_room/tests.rs` with the test relay: dials to a hostless room over 10 min ≤ 4; a presence return makes the next call dial at once; a network error keeps the 60 s cap |
| Old clients | Client-only |
| Effort | S |
| Measure after | M2 for DeviceRoom: `http` and the `clientDisconnected` count return to the Nov 17–20 level (about 300 a day) |

Built on branch `cypher/do-cost-cuts` (2026-11-25), with these choices:

- `LinkCacheConfig::cooldown_offline_max` (600 s). A dial whose last attempt met the
  relay's own `host_offline` (the readiness probe's link closed with that code, or a sealed
  link's handshake got it) counts as "offline" when the peer's presence is stale; the engine
  installs the check with `LinkCache::set_presence_check`, pointing at
  `WorkspaceHost::device_online` (presence or a relay status probe seen within 45 s).
  Without a check, presence counts as stale.
- After an offline failure the next sequence is one dial. **Deviation:** the second offline
  failure in a row goes straight to 600 s rather than doubling up to it: a doubling ladder
  from 5 s dials 3 + 6 times in the first 10 minutes, so the design's own test (≤ 4 dials in
  10 min) needs the jump. The first failure keeps three attempts and 5 s whatever the
  cause. Network errors and failures while presence is fresh keep today's 5 s → 60 s
  ladder, three attempts each.
- Reset, unchanged: `reset_cooldown` (the presence "came back" hook, which a relay status
  probe answering alive also feeds through `presence_seen`), a system wake and a token
  change clear the state. The failure streak now decays 10 min after its cooldown *ended*
  (before: after the last failure), so waiting out the 600 s cooldown does not restart the
  ladder at three attempts.
- The engine's presence hook now holds the cache weakly (the cache holds the workspace for
  the presence check).
- Tests: `crates/rpc/tests/device_room.rs` ("D1" section) against the fake relay, scaled
  1:100 in real time (the relay is real sockets and the cooldown uses `std::time::Instant`,
  so paused time does not apply): 3 dials on the first failure and 4 in the offline cap;
  presence return dials at once and reaches the host; fresh presence and network errors
  keep the short cap (≥ 18 dials in 3 s).
- iOS: `DeviceRelayClient.call` retries 3 times within one call and keeps no cooldown
  across calls, so there is no ladder to change; a phone caller asking a dead peer pays 3
  upgrades per call. Not changed (it needs a presence source on the phone too).

### N1: settle chats the Edge called hidden

A host reports every status transition of every chat to `/notifications/event`
(`crates/engine/src/auth.rs:848`). For a hidden workspace thread or a memory pass the Edge
answers `ignored`, reason `hidden`, and notes that the markers behind that verdict never
change (`apps/edge/src/notifications/notifications.ts:406-409`,
`apps/edge/src/notifications/notifications-model.ts:142-152`). The host still treats each
new transition as a first ignore: three attempts 5 s and 10 s apart
(`crates/engine/src/host/notification_events.rs:188-218`), then retries at 30 s doubling to
600 s (`:66-67`, `:219-233`). A hidden run with two transitions costs at least 6 requests.

Design: carry the reason in the reply (`crates/engine/src/host/notification_events.rs:109`
already reads it for the log); on `hidden`, settle the chat for the life of the process: no
retry, no report for later transitions. Other reasons (`chat`, `archived`, `device`,
`space`) keep today's behaviour, because they can change.

| | |
|---|---|
| Saving | Unmeasured. The whole notification HTTP line is a guess (about 73k a month); hidden threads are frequent since workspaces came into use. **0–40k a month.** M3 gives the `/notifications/event` rate and its `ignored` share |
| Risk | None for delivery: these chats never notify. A chat whose markers did change would stay silent until restart; the Edge's own comment excludes that |
| Tests | `crates/engine/src/host/notification_events/tests.rs`: `hidden` → one request for any number of transitions; `chat` → today's three and the backoff. Edge side already covered by `apps/edge/test/workerd/notification-event-ignored.workerd.test.ts` |
| Old clients | Client-only; the `hidden` reason has been in the reply since `84848c7c` (PR #69) |
| Effort | S |
| Measure after | M3: `/notifications/event` per hour |

### G1 to G3: guards against request storms

What happened:

| Incident | Mechanism in the code | Multiplier | Billed above the floor (estimate) |
|---|---|---|---:|
| Nov 6–11 | A receive-only device treated its own policy refusal as a failed dial and its cooldown was cleared on every presence overlay: about 25 dials a minute (`40afda57`). A Linux service pinned to an old version restarted every 24 s (`df5fe800`): each start is a registry upgrade + pull, a relay upgrade, a trust read. Every refused dial made the host re-read the chain, up to once per 15 s (`crates/engine/src/device_trust.rs:43`, `:887-896`) | TrustRoom ×2, DeviceRoom ×4, registry drops ×15 | 166k in six days: 72k trust + 58k registry + 36k device |
| Oct 26–30 | Not established; same signature without TrustRoom (it did not exist yet). 3,640 ChatRoom exceptions on Oct 29 | registry and device drops ×3–10 | 39k |
| Nov 21–23 | Inferred: one caller dialing one unreachable host at the cooldown cap (D1) | DeviceRoom ×2.6 | 12k in three days, 119k a month if it continues |

| Guard | Design | Code touched | Effect |
|---|---|---|---|
| G1a healthy-session backoff | A registry or chat session resets the backoff to 250 ms only if it stayed joined for 30 s, as the host relay already does (`crates/rpc/src/device_room/relay.rs:134-146`, `:182`); a shorter one keeps doubling to the 30 s cap | `crates/sync/src/registry/actor.rs:137-139`, `crates/sync/src/chat_client/actor.rs:159-161` | A room that accepts and drops costs 2 connects a minute instead of up to a few a second; a healthy session that drops redials in 250 ms as today |
| G1b jitter and wake spacing | Add the relay's jitter (`crates/rpc/src/device_room/relay.rs:184-191`) to registry and chat backoff waits; honour "a sibling dial succeeded" (`crates/net/src/dial.rs:98`) at most once per 10 s per actor, as the probe resets already are (`crates/engine/src/host/workspace_host/presence.rs:38`) | `crates/sync/src/registry/actor.rs:269-293`, `crates/sync/src/chat_client/actor.rs` | A flapping network no longer redials the registry and up to 12 chats on every flap |
| G1c dial budget | A token bucket per room URL in the one function every WebSocket dial goes through: burst 12, then one per 10 s; an exhausted bucket delays the dial, never fails it | `crates/net/src/dial.rs:96-99` | Hard ceiling of 8,640 upgrades a day per room per process, whatever the bug; an engine start (1 registry + 1 relay + up to 12 chats, one each) never touches it |
| G1d miss-refresh backoff | After a refresh that found the chain unchanged, the next refresh for the same unknown device waits 15 s doubling to 300 s; a chain change (T1's hint or any sync) clears it | `crates/engine/src/device_trust.rs:887-896` | The 72k of Nov 6–11 becomes a few thousand; a newly paired device is admitted at the first refresh after the chain grows, as today |
| G2 daily usage guard | Run `python3 scripts/ops/cf-usage.py --json --fail-at 80` once a day from a scheduled workflow with a read-only token, and the M2 query for yesterday: fail when a class's billed requests exceed twice its 7-day median | a workflow; no product code | Nov 6 would have failed on its first day (Worker 89k against 31k); the incident ran six |
| G3 Edge backoff header | Clients honour `retry-after` (capped at 10 min) on any Edge response, including a refused upgrade; the Worker can then slow one path for one client version within minutes of a deploy | `apps/edge/src/router.ts`, `crates/net/src/dial.rs`, the HTTP clients of `crates/engine/src/device_trust.rs:83-93` and `crates/engine/src/bots/network/edge.rs:116` | A bad release is throttled from the Worker instead of waiting for a client release. Each refused request still bills 1 |

No guard changes steady-state requests. Risk: G1c would delay a legitimate burst of more
than 12 dials to one room; the densest one in the code is a peer link's first three
cooldown steps, 9 dials in 30 s (`crates/rpc/src/device_room/links.rs:333-336`). The tests
are the existing reconnect suites with a counting connector. G1 covers the Rust clients;
iOS has its own backoff (`apps/ios/Cypher/Sync/RoomSocketLifecycle.swift:39-40`) and is
suspended in the background, so G1a there is optional. Effort S each, G3 S–M. Old clients
are unaffected by all of them.

### N2: notification reports over the registry socket

`event` and `activity` reports are HTTP because the caller reads the reply (`readEventIds`,
badge, `ignored`; `apps/edge/src/notifications/notifications.ts:373-380`, `:381-473`). A
request/reply frame pair on the registry socket carries the same body at 1/20:
`{t:"notify",id,path,body}` answered by `{t:"notified",id,reply}` to the sender only, when
`state` advertised it; HTTP stays for a client that is not joined
(`crates/engine/src/rpc.rs:605-613`). Touches
`apps/edge/src/registry/registry-room.ts:299-314`, `crates/sync/src/registry/frames.rs`,
`crates/engine/src/host/notification_events.rs`,
`apps/ios/Cypher/Notifications/NotificationController.swift:397-427`.

Saving: 80% of the guessed 73k a month at 19/20 = **up to 55k billed, 0 analytics** (a
request becomes a message). Do it only if M3 shows the line is real after N1. Risk: a reply
lost with the socket must fall back to HTTP with the same idempotency the HTTP retry has
today (`claim`, `apps/edge/src/notifications/notifications.ts:470`). Effort M–L. iOS's
`settings` and `register` at every foreground stay HTTP.

### D2: relay status probe cap by age

`relay_probe_task` asks `GET /device/{id}/status` for devices whose presence is stale,
backing off to 1,800 s for a device that answers offline and 300 s for one that answers
alive (`crates/engine/src/host/workspace_host/presence.rs:18`, `:30`, `:43`): 48 a day per
(engine, long-gone device row), for ever. The floor of DeviceRoom's HTTP is 1,952 a day =
58.6k a month, of which this is an unknown part (M3).

Design: let the offline cap grow with how long the device has been gone: 30 min for the
first day, 2 h up to a week, 6 h after. Presence, a registry reconnect, a foreground retry
and a wake still re-probe at once (`:38`). Saving **up to 25k a month**. It changes one
bound: a device gone for more than a week that returns while its presence cannot reach this
engine is noticed after up to 6 h instead of 30 min. Engine only, effort S.

### P1: presence, one beat per room

The beat is 44% of the analytics count and 8% of the bill (60,586 messages a day = 90.9k
billed a month). It is also the activity lease for notifications (45 s,
`apps/edge/src/notifications/notifications-model.ts:5`), the signal that clears dial and
probe backoffs, and the deaf-socket tripwire
(`crates/engine/src/host/workspace_host/presence.rs:102-123`). A design that removes it has
to keep all four.

Design, using only frame types every client already knows: when RegistryRoom handles a
beat (`apps/edge/src/registry/registry-room.ts:435-457`) it also reads
`getWebSocketAutoResponseTimestamp` of every other ready socket, as DeviceRoom does for its
host (`apps/edge/src/device/device-room.ts:115-130`), and broadcasts
`{t:"presence",device,at}` with `at` = that socket's last ping for each one that pinged in
the last 20 s, including an echo of each device to itself. A new client beats only when it
has an activity report to refresh or has seen no echo of itself for 15 s
(`crates/engine/src/host/workspace_host/sync.rs:229-236`,
`apps/ios/Cypher/Sync/RegistryClient.swift:229-232`). In a room with several devices one
of them beats and the room vouches for the rest.

| | |
|---|---|
| Saving | Engine-equivalents per registry from the object table: 3.2, 2.0, 1.5, 1.4, 0.6, 0.1, 0.05 (8.9). One beater per room: 4.8. −46% of beats: **837k analytics, 42k billed a month** |
| Risk | Medium. A vouched `at` is up to 15 s old when sent, so the margin before the 45 s window drops from two missed beats to one; an old client's handling of an echo of its own device is unchecked. The lease, the backoff resets and the tripwire keep working because presence frames still arrive every 15 s |
| Depends on | M1: the design reads text-ping timestamps. If pings are billed, K1 removes text pings from this socket and P1 falls away (a beat and a ping then cost the same) |
| Old clients | They beat every 15 s as today and each of their beats is a tick for the others. New client on an old Worker: no echoes, beats every 15 s |
| Effort | M: room, engine, iOS; `apps/edge/test/workerd/activity-presence.workerd.test.ts` and `crates/engine/src/host/workspace_host/tests.rs` extended for the 45 s margin |
| Measure after | M2 for RegistryRoom, `type=hibernation`: about 69k a day → about 41k |

### S1: session freshness from host presence

A running session rewrites its registry row every 20 s so that other devices' 45 s
staleness gate keeps showing "Working" (`crates/engine/src/session/engine.rs:1419-1450`):
one push frame, about 5 rows written and one broadcast per device, 120–180 an hour per
live run. The host's presence says the same thing ("this engine is alive") for every one
of its runs at once.

Design: viewers treat a running session as fresh while its host device's presence is
fresh; hosts stop the touch once every device row in the registry reports a version that
does so. Saving: about half of the write-driven RegistryRoom bucket (guessed): **225k
analytics, 11k billed, about 1M rows written a month**. It needs the desktop UI, the iOS
app and the engine changed and a version gate, effort M–L; it is the change that matters
for the rows-written meter at 20× and beyond, not for requests.

### PD1: no badge-only push after an alert

An alert already carries the badge (`apps/edge/src/notifications/notifications.ts:701-706`);
the badge job then sends a badge-only push to every registration, including the phone that
just received that alert (`:722-742`). Skip a recipient whose alert in the same flush
carried the same badge revision. Worker only, effort S, saves at most the badge half of
PushDevice's sends (**up to 3k a month**). PushDevice's 7.6k requests that RegistryRoom's
subrequests do not explain stay open (tail sample on `/unregister`).

### K1: protocol-level keepalive (contingent)

Only if M1 shows that auto-answered text `ping` frames are billed. Every socket sends one
every 15 s (`crates/sync/src/registry.rs:172`, `crates/sync/src/chat_client.rs:330`,
`crates/rpc/src/device_room/links.rs:292`, `crates/rpc/src/device_room/relay.rs:307`,
`apps/ios/Cypher/Sync/RoomSocketLifecycle.swift:282`,
`apps/ios/Cypher/Sync/DeviceRelayClient.swift:376`): 8,640 billed a month per open socket,
about 1.0M a month for the 116 sockets that an invoice of 2.0M would imply (11 per
engine-equivalent: registry, relay and up to 12 warm chats).

Design: send a WebSocket ping control frame instead (`Message::Ping` in tungstenite,
`sendPing(pongReceiveHandler:)` on `URLSessionWebSocketTask`) and count the pong as the
transport lease, same interval, same 45 s lease. The pricing page exempts incoming protocol
pings from requests and the runtime answers them without waking the object (both looked up
through a summarising search tool: confirm on the page). One socket cannot switch: the
DeviceRoom decides whether a host is live from the text ping's auto-response timestamp
(`apps/edge/src/device/device-room.ts:115-130`), which a control frame does not stamp. The
host socket keeps text pings (one socket per engine, 8,640 a month) or the room learns
liveness another way. Client-only otherwise; the Edge answers both kinds already. Effort
S–M. Measure: the next invoice's quantity against `cf-usage.py` for the same cycle.

## Where the requests come from

The attribution the designs above rest on (step 1 of the analysis), kept as written apart
from the [corrections](#corrections-to-the-attribution) listed at its end. Source: one
usage snapshot, 2026-10-24..2026-11-23 (31 days), and the code at `53c34485`. Nothing here
was measured live; the split inside each class is a model fitted to the snapshot
([how](#how-the-classes-were-split)).

- **3,616,140 is the analytics count, not the billed count.** Analytics count every
  inbound WebSocket message as one request; billing folds them 20:1. Billed requests for
  the window are **about 0.99M (plausible 0.9–1.2M)** against 1M included. 3.6M billed
  would have cost $0.39–$0.45 of overage, not $0.15. What the $0.15 on a $5.15 invoice
  says about the billed quantity depends on a rule this analysis could not confirm: charged
  per whole million it means 1–2M billed, which agrees; charged pro rata it means 2.0M,
  which is this estimate plus about 1.0M. One assumption carries the 0.99M: that
  auto-answered `ping` frames are not billed ([open](#verified-vs-inferred)); billed, they
  would add 0.3–1.1M.
- **What bills is HTTP, not the sockets.** About 0.85M of the 0.99M are 1:1 requests
  (HTTP, WebSocket upgrades, alarms). The 2.77M WebSocket messages bill as about 0.14M.
- **RegistryRoom is 62% of the analytics count and about 30% of the bill.** 70% of its
  requests are the 15 s presence beat (1.59M messages, about 79k billed).
- **TrustRoom is 10% of the analytics count and about 36% of the bill**: one
  `GET /trust/chain` every 60 s per running engine and per foregrounded iPhone, 352k
  requests, all 1:1.
- **Connection churn is the second billed line**: each (re)connect of a registry or chat
  socket costs a WebSocket upgrade plus an HTTPS pull (2 billed), a device-room dial costs
  an upgrade each. About 174k sessions ended by a disconnect in the window; with their
  pulls about 290k billed.
- **Two incidents carry a large share**: Nov 6–11 (0.3.70 → 0.4.4) is 41% of all Worker
  requests of the window and doubled TrustRoom; Oct 26–30 looks the same in the data.
- **Run rate without incidents (Nov 17–23): about 36k billed a day, 1.09M a month**:
  TrustRoom 41%, BotRoom inbox poll 10% and rising (1.3k → 4.9k a day in 13 days).
- All sockets are hibernatable and all three socket rooms answer `ping` with
  `setWebSocketAutoResponse`; keepalives are not in the analytics numbers. Duration is
  28,466 GB-s of 400,000 included.

Top sources by billed requests (window of 31 days):

| # | Class | Source | Analytics requests | Billed | Confidence |
|---|---|---|---:|---:|---|
| 1 | TrustRoom | chain poll every 60 s (incl. ~72k incident inflation) | 352k | 352k | measured (class is HTTP only) |
| 2 | RegistryRoom | presence beat every 15 s | 1,587k | 79k | derived |
| 3 | RegistryRoom | notification HTTP (`event`, `activity`, `settings`, `register`) | ~75k | ~75k | guessed |
| 4 | DeviceRoom | WebSocket upgrades (dials, failed dial bursts, host rejoins) | ≥73k | ≥73k | measured lower bound |
| 5 | DeviceRoom | `GET /status` relay probes and other baseline HTTP | ~61k | ~61k | derived |
| 6 | ChatRoom | HTTPS pull at every chat open / redial | ~60k | ~60k | derived |
| 7 | RegistryRoom | WebSocket upgrades | ≥55k | ≥55k | measured lower bound |
| 8 | RegistryRoom | HTTPS pull at every start / drop | ~55k | ~55k | derived |
| 9 | ChatRoom | WebSocket upgrades | ≥45k | ≥45k | measured lower bound |
| 10 | BotRoom | inbox poll every 60 s per connected bot | 37k | 37k | measured (class is HTTP only) |
| 11 | RegistryRoom | push frames (session freshness touch, row writes) | ~390k | ~20k | derived |
| 12 | ChatRoom | push frames | ~257k | ~13k | derived |

## Billing rules

From the [Durable Objects pricing page](https://developers.cloudflare.com/durable-objects/platform/pricing/)
(looked up twice on 2026-11-24 through a search tool that returns a summary, not the page
text) and this repository's own measurements. The two lookups disagree on the rounding rule,
and neither found a sentence on auto-responses as requests: those two rows are to settle
from the invoice (M1).

| Item | Billed as | Source |
|---|---|---|
| `fetch()` to a DO stub (from the Worker or from another DO) | 1 request | pricing page |
| RPC method call on a stub | 1 request per RPC session | pricing page; the Edge uses none, every call is `stub.fetch` |
| WebSocket upgrade | 1 request | pricing page |
| Incoming WebSocket message | 1/20 request | pricing page, footnote on the 20:1 ratio |
| Outgoing WebSocket message | free | pricing page |
| `ping` answered by `setWebSocketAutoResponse` | no wake and no duration (pricing page). As a request: **not stated; assumed unbilled here**. For: it produces no invocation (`docs/operations/cloudflare-billing.md`: 20 pings, 0 spans) and is absent from analytics. Against: the page's footnote exempts "incoming WebSocket protocol pings" by name, a text `ping` frame is not one, and the sentence about auto-responses stands under duration | to confirm against an invoice |
| WebSocket protocol ping (control frame) | free, answered by the runtime without waking the object | pricing page footnote and the WebSockets page, as summarised by the second lookup |
| Alarm invocation | 1 request; `setAlarm` also bills one row written | pricing page |
| Included / overage | 1M requests a month, then $0.15 per million. **Whole millions or pro rata: not confirmed.** The first lookup reported rounding up to the next million; the second found no such sentence and a worked example on the page that prorates (1.5M requests → $0.075). An invoice line of exactly $0.15 is what whole-million charging produces for any quantity between 1M and 2M, and proration only at about 2.0M | pricing page; to confirm against an invoice |
| Duration | wall-clock GB-s at 128 MB while running or unable to hibernate; nothing while hibernated or eligible; 400,000 GB-s included | pricing page |
| `ws.accept()` socket | duration for the whole connection | pricing page; not used anywhere in `apps/edge/src` |
| Rows | 50M written and 25B read a month included; index updates and deletes count as rows written | pricing page |

What the snapshot's metrics count:

| Metric | Counts | Basis |
|---|---|---|
| `durableObjectsInvocationsAdaptiveGroups.requests` | HTTP + alarms + **every** hibernation event (inbound message), raw, not 20:1. Sampled: every value is a multiple of 10. | Pricing page ("the 20:1 ratio does not affect metrics"); `scripts/ops/cf-usage.py:103-109` splits it by the `type` dimension (`http`, `alarm`, `hibernation`); the fit below (4.0 RegistryRoom requests per TrustRoom poll = four 15 s beats per 60 s poll) |
| ... and auto-answered `ping` | **not counted** | DeviceRoom had 2,370 requests on Nov 15 with several host sockets each pinging 5,760 times a day |
| `durableObjectsPeriodicGroups.inboundWebsocketMsgCount` | **0 for every class on every day**: not populated for hibernatable sockets here | snapshot |
| `outboundWebsocketMsgCount` | populated (4.97M) | snapshot |
| `activeTime` | microseconds; GB-s = s × 0.125 | `more.py` beside the snapshot |
| status `clientDisconnected` / `responseStreamDisconnected` | appear only on the three socket classes; read here as "a WebSocket session ended by a drop" (one per session) | inferred: HTTP-only classes show `success` only; Worker-level total 182,820 vs DO-level 173,820 |

`billable = http + alarm + hibernation / 20`. The snapshot was taken without the `type`
dimension, so that split is modelled below, not read.

## Snapshot summary

Data: `~/.cypher-development/cf-usage/snapshot-2026-11-24.json` (outside the repository),
produced by `snap.py` beside it. All seven namespaces belong to script `cypher-edge` and are
SQLite-backed. SessionRoom (410 stub) has no requests.

### Totals by class and status

| Class | Requests | Share | success | clientDisconnected | responseStreamDisconnected | scriptThrewException | internalError |
|---|---:|---:|---:|---:|---:|---:|---:|
| RegistryRoom | 2,250,890 | 62.2% | 2,195,380 | 53,650 | 1,730 | 120 | 10 |
| ChatRoom | 619,440 | 17.1% | 569,940 | 37,900 | 7,080 | 4,520 | 0 |
| TrustRoom | 352,030 | 9.7% | 352,030 | 0 | 0 | 0 | 0 |
| DeviceRoom | 339,670 | 9.4% | 266,140 | 70,850 | 2,610 | 70 | 0 |
| BotRoom | 36,850 | 1.0% | 36,850 | 0 | 0 | 0 | 0 |
| PushDevice | 17,260 | 0.5% | 17,260 | 0 | 0 | 0 | 0 |
| **Total** | **3,616,140** | 100% | 3,437,600 | 162,400 | 11,420 | 4,710 | 10 |

Worker `cypher-edge`: 1,198,260 requests (success 1,011,600, clientDisconnected 94,870,
responseStreamDisconnected 87,950, scriptThrewException 3,840), 1,220,170 subrequests,
2,537 s CPU. `cypher-www-redirect`: 7,090. 3,640 of ChatRoom's 4,520 exceptions fall on
Oct 29.

### Periodic statistics

| Class | Active time (s) | Duration (GB-s) | CPU (s) | Outbound WS msgs | Rows read | Rows written | Subrequests | CPU ms / request |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| RegistryRoom | 21,056 | 2,632 | 1,532 | 3,674,372 | 12,274,266 | 1,740,878 | 9,628 | 0.68 |
| ChatRoom | 19,548 | 2,443 | 606 | 1,129,620 | 13,558,124 | 768,224 | 0 | 0.98 |
| TrustRoom | 1,050 | 131 | 214 | 0 | 2,150,742 | 146 | 0 | 0.61 |
| DeviceRoom | 180,735 | 22,592 | 153 | 164,418 | 242,864 | 193 | 0 | 0.45 |
| BotRoom | 391 | 49 | 71 | 0 | 615,687 | 376 | 80 | 1.94 |
| PushDevice | 4,946 | 618 | 30 | 0 | 2,077,244 | 27,390 | 7,551 | 1.73 |
| **Total** | 227,726 | 28,466 | 2,606 | 4,968,410 | 30,918,927 | 2,537,207 | 17,259 | |

Against the monthly inclusions: duration 7%, rows written 5%, rows read 0.1%. DeviceRoom
holds 79% of the duration, concentrated on Oct 26–29 (15–35k s a day) and Nov 6
(18k s); the cause is not established.

### Objects

The top-200 query returns every object with at least 1,040 requests.

| Class | Object is | Name | Objects ≥ 1,040 requests | Share of class | Largest | Top 3 |
|---|---|---|---:|---:|---:|---:|
| RegistryRoom | one user's registry in one org | `reg1/{orgId}/{userId}` (`apps/edge/src/room-routes.ts:71`) | 7 | 100.0% | 812,280 (36.1%) | 75.1% |
| TrustRoom | one user | `t1/{userId}` (`apps/edge/src/trust/trust-route.ts:13`) | 6 | 100.0% | 117,290 (33.3%) | 80.4% |
| DeviceRoom | one device (engine) | `d2/{deviceId}` (`apps/edge/src/room-routes.ts:90`) | 13 | 99.5% | 107,090 (31.5%) | 55.9% |
| ChatRoom | one chat | `chat2/{chatId}` (`apps/edge/src/room-routes.ts:51`) | 167 | 68.2% | 26,090 (4.2%) | 7.3% |
| BotRoom | one networked bot | `bot1/{botRoomId}` (`apps/edge/src/room-routes.ts:115`) | 5 | 100.0% | 10,300 (28.0%) | 69.0% |
| PushDevice | one APNs token | `apns/{environment}/{token}` (`apps/edge/src/notifications/notifications.ts:489`) | 2 | 94.1% | 14,150 (82.0%) | 94.1% |

Population inferred from this: 6 users, 7 registries, at least 13 device rooms, 5 networked
bots, 2 phones with push, several hundred chats (the 31.8% of ChatRoom outside the list is
at least 190 chats). TrustRoom at one poll a minute gives **engine-equivalents online**
(engines plus foregrounded phones): 11,680 polls a day on non-incident days = 8.1; 14,886 a
day in Nov 17–23 = 10.3; the largest user alone 3.4. The largest RegistryRoom object fits
the same user: 3.4 engines × 5,760 beats × 31 days = 607k of its 812k.

### By day

`Disc.` = requests with status clientDisconnected or responseStreamDisconnected
(RegistryRoom / ChatRoom / DeviceRoom).

| Date | Registry | Chat | Trust | Device | Bot | Push | DO total | Worker | DO/Worker | Disc. R / C / D | Rows written R / C |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10-24 Sat | 74,000 | 28,720 | 0 | 6,250 | 0 | 1,090 | 110,060 | 11,770 | 9.4 | 530 / 2,080 / 500 | 56,318 / 34,685 |
| 10-25 Sun | 73,760 | 23,690 | 0 | 18,260 | 0 | 1,530 | 117,240 | 13,030 | 9.0 | 600 / 2,420 / 420 | 59,277 / 29,610 |
| 10-26 Mon | 88,620 | 29,780 | 0 | 14,590 | 0 | 1,120 | 134,110 | 27,560 | 4.9 | 1,890 / 2,150 / 1,760 | 81,857 / 41,809 |
| 10-27 Tue | 93,750 | 45,130 | 0 | 20,270 | 0 | 1,300 | 160,450 | 43,470 | 3.7 | 3,250 / 3,850 / 3,350 | 87,146 / 57,320 |
| 10-28 Wed | 105,700 | 32,530 | 0 | 12,230 | 0 | 520 | 150,980 | 45,950 | 3.3 | 3,450 / 1,700 / 3,680 | 93,799 / 46,036 |
| 10-29 Thu | 94,550 | 30,600 | 0 | 21,840 | 0 | 80 | 147,070 | 49,290 | 3.0 | 3,960 / 2,510 / 4,520 | 63,578 / 18,632 |
| 10-30 Fri | 83,340 | 23,230 | 0 | 6,200 | 0 | 540 | 113,310 | 31,730 | 3.6 | 2,420 / 1,110 / 2,360 | 84,579 / 31,211 |
| 10-31 Sat | 47,430 | 17,270 | 4,660 | 4,240 | 0 | 1,270 | 74,870 | 16,220 | 4.6 | 290 / 1,550 / 340 | 53,090 / 18,653 |
| 11-01 Sun | 39,230 | 7,950 | 7,400 | 1,170 | 0 | 270 | 56,020 | 12,450 | 4.5 | 130 / 570 / 170 | 21,774 / 11,175 |
| 11-02 Mon | 50,420 | 20,080 | 9,210 | 8,010 | 0 | 560 | 88,280 | 19,550 | 4.5 | 370 / 1,590 / 260 | 43,651 / 30,429 |
| 11-03 Tue | 65,110 | 24,570 | 10,040 | 5,070 | 0 | 630 | 105,420 | 20,710 | 5.1 | 390 / 1,730 / 300 | 72,422 / 30,279 |
| 11-04 Wed | 50,780 | 22,120 | 7,070 | 8,320 | 0 | 640 | 88,930 | 18,960 | 4.7 | 320 / 1,570 / 1,980 | 60,985 / 32,515 |
| 11-05 Thu | 53,240 | 9,670 | 10,650 | 3,790 | 0 | 60 | 77,410 | 31,520 | 2.5 | 1,640 / 740 / 1,200 | 36,411 / 11,137 |
| 11-06 Fri | 101,460 | 15,420 | 24,640 | 25,990 | 0 | 290 | 167,800 | 88,990 | 1.9 | 6,240 / 600 / 7,560 | 80,859 / 31,876 |
| 11-07 Sat | 89,900 | 4,480 | 25,220 | 30,360 | 0 | 140 | 150,100 | 84,340 | 1.8 | 5,320 / 390 / 7,800 | 49,930 / 3,617 |
| 11-08 Sun | 79,220 | 6,360 | 18,760 | 7,130 | 0 | 310 | 111,780 | 53,750 | 2.1 | 3,020 / 450 / 3,140 | 42,963 / 7,053 |
| 11-09 Mon | 100,910 | 17,750 | 24,470 | 18,890 | 0 | 810 | 162,830 | 78,290 | 2.1 | 4,620 / 490 / 5,900 | 88,671 / 35,434 |
| 11-10 Tue | 97,580 | 16,420 | 25,120 | 24,660 | 0 | 240 | 164,020 | 89,230 | 1.8 | 5,680 / 560 / 6,630 | 84,139 / 43,997 |
| 11-11 Wed | 113,720 | 20,420 | 23,570 | 19,720 | 1,280 | 940 | 179,650 | 98,060 | 1.8 | 6,660 / 1,450 / 6,540 | 91,808 / 31,160 |
| 11-12 Thu | 60,100 | 11,280 | 12,780 | 5,870 | 1,940 | 220 | 92,190 | 35,130 | 2.6 | 1,210 / 790 / 1,310 | 36,384 / 12,286 |
| 11-13 Fri | 55,410 | 10,710 | 13,580 | 3,200 | 2,490 | 20 | 85,410 | 27,520 | 3.1 | 170 / 890 / 160 | 18,163 / 4,959 |
| 11-14 Sat | 44,770 | 7,840 | 11,230 | 2,660 | 1,080 | 40 | 67,620 | 22,300 | 3.0 | 200 / 630 / 80 | 11,946 / 2,710 |
| 11-15 Sun | 40,890 | 6,340 | 10,430 | 2,370 | 2,000 | 80 | 62,110 | 20,000 | 3.1 | 80 / 250 / 0 | 9,784 / 3,082 |
| 11-16 Mon | 38,220 | 12,270 | 9,000 | 4,140 | 1,460 | 190 | 65,280 | 20,850 | 3.1 | 240 / 980 / 250 | 23,519 / 12,014 |
| 11-17 Tue | 65,610 | 22,230 | 13,150 | 6,010 | 2,460 | 630 | 110,090 | 31,240 | 3.5 | 580 / 2,000 / 220 | 44,452 / 24,321 |
| 11-18 Wed | 67,840 | 30,340 | 13,470 | 6,180 | 2,570 | 600 | 121,000 | 34,360 | 3.5 | 460 / 3,180 / 380 | 58,065 / 24,489 |
| 11-19 Thu | 66,390 | 22,210 | 13,920 | 4,920 | 3,290 | 350 | 111,080 | 31,810 | 3.5 | 420 / 2,130 / 320 | 42,071 / 17,854 |
| 11-20 Fri | 82,000 | 30,900 | 16,350 | 4,700 | 4,000 | 510 | 138,460 | 34,260 | 4.0 | 330 / 1,700 / 290 | 68,743 / 35,567 |
| 11-21 Sat | 63,660 | 13,270 | 14,920 | 9,850 | 4,740 | 450 | 106,890 | 32,660 | 3.3 | 350 / 1,420 / 2,450 | 24,919 / 10,456 |
| 11-22 Sun | 73,970 | 23,430 | 15,490 | 21,390 | 4,660 | 780 | 139,720 | 36,840 | 3.8 | 280 / 1,910 / 6,350 | 56,202 / 25,487 |
| 11-23 Mon | 89,310 | 32,430 | 16,900 | 11,390 | 4,880 | 1,050 | 155,960 | 36,420 | 4.3 | 280 / 1,590 / 3,240 | 93,373 / 48,371 |

Per-day averages:

| Period | Registry | Chat | Trust | Device | Bot | Push | DO total | Worker |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Oct 24–30 | 87,674 | 30,526 | 0 | 14,234 | 0 | 883 | 133,317 | 31,829 |
| Oct 31–Nov 5 | 51,035 | 16,943 | 8,172 | 5,100 | 0 | 572 | 81,822 | 19,902 |
| Nov 6–11 | 97,132 | 13,475 | 23,630 | 21,125 | 213 | 455 | 156,030 | 82,110 |
| Nov 12–16 | 47,878 | 9,688 | 11,404 | 3,648 | 1,794 | 110 | 74,522 | 25,160 |
| Nov 17–23 | 72,683 | 24,973 | 14,886 | 9,206 | 3,800 | 624 | 126,171 | 33,941 |
| October part (8 days) | 82,644 | 28,869 | 582 | 12,985 | 0 | 931 | 126,011 | 29,878 |
| November part (23 days) | 69,119 | 16,891 | 15,103 | 10,252 | 1,602 | 427 | 113,393 | 41,706 |

Weekday pattern: ChatRoom and RegistryRoom's write traffic follow working days (ChatRoom
6–8k on the weekend of Nov 14–15, 22–32k on Nov 17–20); the presence and poll floors do
not. Worker requests by hour on Nov 22–23 are flat, 1,250–1,892 an hour (mean 1,522), day
and night: the Worker mostly serves fixed-interval polls (TrustRoom alone 645–704 an hour,
BotRoom 194–203).

### Worker requests against DO requests

DO requests are 1.8–9.4 times Worker requests each day, so most DO requests never passed
the Worker as a request: they are WebSocket messages. Decomposition of the Worker's
1,198,260:

| Part | Requests | How obtained |
|---|---:|---|
| TrustRoom forwards | 352,030 | class total (HTTP only) |
| BotRoom forwards | ~36,800 | class total less ~80 DO-to-DO calls and alarms |
| DeviceRoom HTTP | ~134,000–175,000 | class total less relayed frames (≈ outbound messages) |
| RegistryRoom HTTP + ChatRoom HTTP | ~290,000 | modelled below |
| Not a DO request (`/auth/*`, `/releases/*`, 401, 404, `/health`) | ~345,000–385,000 | remainder |

The remainder `Worker − Trust − Bot − DeviceHTTP` fits
`7.95 × RegistryDisc + 2.0 × ChatDisc + 3,333 a day` (R² 0.987): a registry session that
ends by a drop comes with about eight Worker requests, of which RegistryRoom receives 3.4
(next section). The rest is what an engine start does besides: token refresh, update check
and download.

### Step changes

| Date | Change in the data | Nearest change in the code | Status |
|---|---|---|---|
| Oct 26–30 | Worker 27–49k a day (11–13k before), disconnect-ended sessions ×3–10 on RegistryRoom and DeviceRoom, DeviceRoom duration 15–35k s a day | releases 0.3.63–0.3.66 on Oct 26–29 | cause not established; same signature as Nov 6–11 |
| Oct 31 | TrustRoom starts (4.7k → 7–10k a day) | `64922a04` (TrustRoom, Oct 30), 0.3.67 (Oct 31) | established |
| Oct 31–Nov 5 | RegistryRoom 88k → 51k a day | none found; Oct 31–Nov 1 is a weekend and engine-equivalents are 5–7 | inferred: fewer clients online |
| Nov 6–11 | Worker 82k a day (×4), TrustRoom 23.6k (×2), DeviceRoom 21k (×4), 3.0–6.7k registry sessions dropped a day | 0.3.70 (`b3f8b4ac`, Nov 6 04:15): receive-only devices. Fixed by `40afda57` (0.4.4, Nov 12 09:46: a receive-only device dialed its peer about 25 times a minute) and `df5fe800` (0.4.3 for Linux, Nov 12: a service pinned to an old version restarted in a loop) | established for the dial loop (commit message measures it); the restart loop's share is inferred |
| Nov 11 | BotRoom starts, 1.3k → 4.9k a day by Nov 23 | 0.4.0 (Nov 11): the bot network | established |
| Nov 12–13 | everything falls to the floor (Worker 98k → 27.5k) | 0.4.3, 0.4.4 | established |
| Nov 17–23 | RegistryRoom 66–89k, ChatRoom 22–32k on working days; engine-equivalents 9 → 11.7 | 0.4.5–0.4.8; workspaces in use | inferred: more use |
| Nov 21–23 | DeviceRoom 9.9k / 21.4k / 11.4k with 2.4k / 6.3k / 3.2k dropped sessions, RegistryRoom drops unchanged (~300) | `634ffa02` (Nov 21 15:21: a host that rejoins sends every client `host_closed`), 0.4.7, 0.4.8 | cause not established; device-room only. Inferred in step 2: the three-day mean of 4,013 dropped sessions a day is the rate of one client dialing one unreachable host at the cooldown cap (3 dials per 64.5 s = 4,019 a day; [D1](#d1-peer-dial-cooldown-that-follows-presence)) |

Share of the incidents: Nov 6–11 has 492,660 Worker requests (41% of the window) and
141,780 TrustRoom requests (about 72k above the floor). Without those six days the
November part averages 27.4k Worker requests a day, below the October part.

## How the classes were split

Least-squares fits of each class's daily requests on daily drivers that the snapshot does
contain. `Disc` is the class's disconnect-status count, `rowsW` its rows written.

| Class | Fit | Days | R² | Reading |
|---|---|---|---:|---|
| RegistryRoom | `4.07 × Trust + 0.272 × rowsW + 3.43 × Disc − 4,660` | Nov 12–23 | 0.995 | 4 presence beats per trust poll (15 s against 60 s, same clients); 3.7 rows written per write-driven request; 3.4 requests per reconnect |
| RegistryRoom | `4.02 × Trust + 0.282 × rowsW − 3,108` | Nov 12–23 | 0.991 | same, without the reconnect term |
| ChatRoom | `0.346 × rowsW + 6.68 × Disc + 1,716` | all 31 | 0.939 | 2.9 rows written per write-driven request; 6.7 requests per session; 1.7k a day of floor |
| ChatRoom | `0.471 × rowsW + 4.82 × Disc + 2,892` | Nov 12–23 | 0.978 | 4.8 requests per session; 2.9k a day of floor |
| DeviceRoom | `1.83 × Disc + 0.878 × outbound + 1,952` | all 31 | 0.987 | upgrade plus close event per session; one inbound frame per outbound; 2.0k a day of floor |

Applied to the window:

| Class | Bucket | Requests |
|---|---|---:|
| RegistryRoom | per reconnect: 3.43 × 55,380 | 189,953 |
| | write-driven: 0.272 × 1,740,878 | 473,519 |
| | rest: presence beats (= 276 engine-days, 8.9 engine-equivalents; TrustRoom gives 8.1–10.3) | 1,587,418 |
| ChatRoom | write-driven: 0.346 × 768,224 | 265,806 |
| | per session: 6.68 × 44,980 | 300,511 |
| | floor: 1,716 × 31 | 53,196 |
| DeviceRoom | upgrades: 73,460; close and other events: 0.83 × 73,460 | 134,726 |
| | relayed frames: 0.878 × 164,418 | 144,359 |
| | floor: 1,952 × 31 | 60,512 |

Limits: Nov 6–11 inflates TrustRoom, so the RegistryRoom fit holds only outside it; before
Oct 31 there is no TrustRoom to check the presence bucket against; a
session that closes cleanly has status `success` and lands in the floor; an HTTP request
that follows writes (a notification event) is inside "write-driven".

## Attribution

One row per class and source. Requests are for the 31-day window; `Billed` applies 1:1 to
HTTP, upgrades and alarms and 20:1 to WebSocket messages. Confidence: **measured** = read
from the snapshot; **derived** = fit or code arithmetic that reproduces the snapshot;
**guessed** = an estimate the snapshot cannot check.

### RegistryRoom: 2,250,890 analytics, about 299k billed

Edge: `apps/edge/src/registry/registry-room.ts`. Hibernatable accept (`:182`),
auto-response (`:87`), handlers `webSocketMessage` (`:282`), `webSocketClose` (`:317`).
Clients: `crates/sync/src/registry.rs`, `crates/sync/src/registry/actor.rs`,
`crates/engine/src/host/workspace_host/`, `apps/ios/Cypher/Sync/RegistryClient.swift`.

| Source | Edge | Client | Trigger and frequency | Requests | Class share | Total share | Billed | Confidence | Notes for reduction |
|---|---|---|---|---:|---:|---:|---:|---|---|
| Presence beat (WS message) | `handlePresence` `:435`; rebroadcast to every other socket | `PRESENCE_INTERVAL_MS` `crates/engine/src/host/workspace_host.rs:78`; tick `workspace_host/sync.rs:229`; frame `registry/actor.rs:424`; iOS `RegistryClient.swift:41`, `:229` | every 15 s per joined client: 5,760 a day; carries the viewport activity refresh | 1,587k | 70.5% | 43.9% | 79k | derived | 20:1 already; wakes the room every 15 s per client; freshness window 45 s (`PRESENCE_FRESH_MS` `workspace_host/presence.rs:9`), server TTL memory only |
| Push frame (WS message) | `handlePush` `:352`, `applyPushBatch` `:368`: rows + `seq` + notification observe + broadcast to every socket | `nudge` → `push_pending` `registry/actor.rs:399`, `:480` | one per local mutation batch. Largest steady writer: session freshness touch, every 20–30 s per live run (`touch_session` `crates/engine/src/session/engine.rs:1424`, throttle 20 s `:1431`, tick 15 s `session/engine/run_task.rs:352`, `:468`) = 120–180 an hour per running session | ~390k | 17.3% | 10.8% | ~20k | derived (bucket), guessed (split from the two rows below) | each is ~3.7 rows written and one broadcast per connected device; 1.74M rows written in the window |
| WebSocket upgrade | `fetch` `/ws` `:179` | dial `registry/actor.rs:63`; join retry `workspace_host/sync.rs:33`, `:152`; iOS `RegistryClient.swift:202`, kick on foreground `:184`, `apps/ios/Cypher/App/AppModel+Data.swift:360` | every start, wake (`crates/net/src/wake.rs:31`), drop (silence lease 45 s `registry.rs:41`), probe timeout, redial; backoff 250 ms → 30 s (`registry.rs:52-53`), reset by any successful dial in the process (`crates/net/src/dial.rs:98`) | ≥55.4k | 2.5% | 1.5% | ≥55.4k | measured lower bound | 49k of the 55k fall on Oct 26–30 and Nov 5–12 |
| HTTPS pull beside the socket: `GET /rows?since&beat=1`, `POST /push` per pending batch | `:209`, `:237` | `spawn_offline_sync` `registry/actor.rs:154`, called at start `:52`, on dial failure `:88` `:103`, after every dropped session `:140`, on a local write while not joined `:70` `:91`; transport `workspace_host/transport.rs:37`, `:71`; iOS poll at start and every 20 s while not joined (`apps/ios/Cypher/Sync/RoomSocketLifecycle.swift:41`, `:106`, `RegistryClient.swift:125`) | one pull per connect cycle, before the socket answers; more while offline | ~55k | 2.5% | 1.5% | ~55k | derived (code: 1 per cycle; fit: 3.4 requests per cycle) | the socket's `hello` returns the same rows a moment later |
| `hello`, first push, close event (WS) | `handleHello` `:327`, `webSocketClose` `:317` | `registry/actor.rs:312` | per session | ~79k | 3.5% | 2.2% | ~4k | derived | |
| Notification HTTP | `Notifications.fetch` `apps/edge/src/notifications/notifications.ts:359`: `event` `:381`, `activity` `:373`, `settings` `:360` `:367`, `register` `:475`, `unregister` `:502`; route `apps/edge/src/room-routes.ts:74` | `event`: `crates/engine/src/auth.rs:848`, deduped per status signature, 3 tries then 30 s → 600 s for an ignored chat (`crates/engine/src/host/notification_events.rs:66-67`, `:196`). `activity`: desktop only without a live socket (`crates/engine/src/rpc.rs:605-613`, `auth.rs:807`); iOS every transition (`apps/ios/Cypher/Notifications/NotificationController.swift:397-427`). `settings` + `register`: iOS at bind and every foreground (`:198`, `:235`) | per session status transition on the host; per iOS foreground about 2 + one per chat opened | ~75k | 3.3% | 2.1% | ~75k | guessed | inside the write-driven bucket; 1:1 because the caller reads the reply (`readEventIds`, badge, `ignored`) |
| Alarm | `scheduleAlarm` `:503`, `alarm` `:535`; `Notifications.flush` `notifications.ts:641`, `flushBadge` `:722` | none | 10 s after a notice is queued (`NOTICE_DELAY_MS`), 15 s re-defer while a desktop views the chat, each badge change, daily backup + tombstone GC per dirty room | ~10k | 0.4% | 0.3% | ~10k | guessed (RegistryRoom made 9,628 subrequests: its PushDevice calls plus daily R2 puts) | |
| Quiet probe (WS) | `:309` | 900 s without a protocol frame: `registry.rs:61`, `workspace_host/sync.rs:13`, `registry/actor.rs:443` | ≤96 a day per socket, none while another client's beats arrive | in presence row | | | | derived | |
| `GET /stats`, repair `GET /rows`, `POST /reset` | `:188`, `:209`, `:254` | operator | manual | ~0 | | | | guessed | |

### ChatRoom: 619,440 analytics, about 139k billed

Edge: `apps/edge/src/chat/chat-room.ts`. Hibernatable accept (`:118`), auto-response
(`:97`), `webSocketMessage` (`:346`). Clients: `crates/sync/src/chat_client.rs`,
`crates/sync/src/chat_client/actor.rs`, `crates/engine/src/host/doc_host/chat2_sync.rs`,
`crates/engine/src/host/chat2_host.rs`, `apps/ios/Cypher/Sync/ChatRoomClient.swift`.

| Source | Edge | Client | Trigger and frequency | Requests | Class share | Total share | Billed | Confidence | Notes for reduction |
|---|---|---|---|---:|---:|---:|---:|---|---|
| Push frame (WS message) | `handlePush` `:433`: one log row + `headSeq`, relay to the other sockets | `enqueue` `chat_client.rs:782`: coalesced into the unsent tail, flushed after 2 s (`:824`, `:858`) or at a boundary (`flush_pending` `:865`); one in flight (`chat_client/actor.rs:682`) | while a chat streams, at most one per 2 s and per ack round trip | ~257k | 41.5% | 7.1% | ~13k | derived | 20:1 already |
| WebSocket upgrade | `fetch` `/ws` `:110` | engine: `spawn_chat2_join` `doc_host/chat2_sync.rs:47`, `:117` on every doc open (`doc_host/open.rs:211`); at most 12 warm docs (`doc_host/cache.rs:9`), the least recently used evicted past that once idle 30 s (`:24`). iOS: 8 live chats (`apps/ios/Cypher/App/AppModel.swift:39`), all kicked on every foreground and network change (`AppModel+Data.swift:360`, `ChatRoomClient.swift:330`) | each chat open, each eviction and reopen, each wake, drop or redial | ≥45.0k | 7.3% | 1.2% | ≥45.0k | measured lower bound | |
| HTTPS pull beside the socket: `GET /rows`, `POST /rows` per pending batch, `GET /checkpoint` when the frontier is not held | `:171`, `:237`, `:141` | pull-first: `chat_client/actor.rs:68` at every client start, `:108` `:123` `:162` after every failed dial or dropped session, `http_sync_once` `:896` (`:923`, `:941`); transport `chat2_host.rs:317`, `:352`, fetcher `:202`; iOS `RoomSocketLifecycle.swift:106`, `ChatRoomClient.swift:167` | one per open and per reconnect | ~60k | 9.7% | 1.7% | ~60k | derived (code: ≥1 per session; fit: 4.8–6.7 requests per session) | the socket's `hello` + `rowsReq` return the same rows |
| `hello`, `rowsReq`, gap repair, close event (WS) | `handleHello` `:392`, `handleRowsReq` `:419` | `chat_client/actor.rs:268`, `:361`, `:624` | per session, including sessions that closed cleanly | ~196k | 31.6% | 5.4% | ~10k | derived | |
| Quiet probe (WS) | `:374` | 900 s without progress per open socket (`chat_client.rs:48`, `chat_client/actor.rs:590`); iOS same (`RoomSocketLifecycle.swift:37`) and one probe per joined room on every foreground | ≤96 a day per warm chat socket | ~53k | 8.6% | 1.5% | ~3k | derived (floor of the fit) | |
| Checkpoint `POST /checkpoint` | `:124` | host only: past 200 rows or 512 KB (`doc_host/chat2_sync.rs:276`, `:292`), bootstrap, room reset, rejected push (`:303`) | about one per 200 pushes | ~4k | 0.6% | 0.1% | ~4k | guessed | |
| Alarm | `markBackupDirty` `:568` (`setAlarm` `:577`), `alarm` `:583` | none | once a day per chat written that day: R2 backup | ~5k | 0.8% | 0.1% | ~5k | guessed | |
| `PUT /tail`, `PUT /diff`, `GET /stats`, `POST /reset` | `:277`, `:302`, `:318` | no current client publishes a tail (`doc_host/chat2_sync.rs:271`) | old hosts, operator | ~0 | | | | guessed | |

The stream preview frames in `crates/sync/src/preview_link.rs` are sent only by a
development engine against a development Edge (`crates/engine/src/runtime.rs:325`); this
ChatRoom does not know them.

### TrustRoom: 352,030 analytics, 352,030 billed

Edge: `apps/edge/src/trust/trust-room.ts` (HTTP only, no socket, no alarm), route
`apps/edge/src/trust/trust-route.ts:13`.

| Source | Edge | Client | Trigger and frequency | Requests | Class share | Total share | Billed | Confidence | Notes for reduction |
|---|---|---|---|---:|---:|---:|---:|---|---|
| Background chain resync `GET /trust/chain` | `:68-69` (reads the whole log: 6.1 rows a request) | engine: `spawn_sync` `crates/engine/src/device_trust.rs:355`, `RESYNC` 60 s `:41`, `sync_locked` `:505`; failure backoff 5 s → 300 s. iOS: every 60 s while the app runs, `apps/ios/Cypher/Sync/DeviceTrustStore.swift:262-267` | 1,440 a day per running engine, whether or not anything changed | ~280k | 79.5% | 7.7% | ~280k | derived (11,680 a day on the 18 non-incident days) | the largest billed line; TrustRoom wrote 146 rows in the window (chain entries and pairing mailboxes), against 352k reads |
| Refresh after an unknown client, and boots | same | `refresh_after_miss` `device_trust.rs:887`, at most one per 15 s per host (`MISS_REFRESH` `:43`); one sync per engine start | Nov 6–11: a refused peer dialing a host, and restart loops | ~72k | 20.5% | 2.0% | ~72k | derived (141,780 in Nov 6–11 less six floor days) | gone since 0.4.4 unless a dial loop returns |
| Pairing mailbox poll, chain append | `:74-110`, `append` `:114` | every 2 s while a pairing waits (`POLL` `:38`, `wait_for_join` `:620`, `wait_for_welcome` `:750`), at most 10–15 min; iOS `DeviceTrustStore.swift:142` | per pairing | <1k | | | <1k | guessed | |

### DeviceRoom: 339,670 analytics, about 144k billed

Edge: `apps/edge/src/device/device-room.ts`. Hibernatable accept with tags (`:160` host,
`:171` client), auto-response (`:92`), `webSocketMessage` (`:289`), `webSocketClose`
(`:321`). Clients: `crates/rpc/src/device_room/relay.rs` (host),
`crates/rpc/src/device_room/links.rs` (client),
`apps/ios/Cypher/Sync/DeviceRelayClient.swift`.

| Source | Edge | Client | Trigger and frequency | Requests | Class share | Total share | Billed | Confidence | Notes for reduction |
|---|---|---|---|---:|---:|---:|---:|---|---|
| WebSocket upgrade | `fetch` `/ws` `:138`; a client join is accepted even with no host | host: one socket per engine, `relay.rs:81`, `:259`; rejoin 250 ms after a healthy session (`:178`), wake, token change. Client: `LinkCache::client` `links.rs:438` for any RPC aimed at another device (`crates/engine/src/rpc.rs:388`); 3 dials per failed call (`DIAL_ATTEMPTS` `:30`, 1.5 s and 3 s apart `:336`), then a cooldown 5 s → 60 s (`:333-334`); each dial sends one `ListHarnesses` probe (`:600`). iOS: `DeviceRelayClient.swift:148`, `:183` | per dial; a peer that is unreachable while something keeps asking costs up to 3 upgrades a minute | ≥73.5k | 21.6% | 2.0% | ≥73.5k | measured lower bound | 16k on Oct 26–30, 42k on Nov 4–12 (the fixed dial loop), 12k on Nov 21–23 (unexplained) |
| Relayed frames (WS messages) | `webSocketMessage` `:289`: every client frame and every host reply is an inbound message | RPC calls, streams, terminals over the relay; iOS relay probe after 3 s of silence while a call waits (`DeviceRelayClient.swift:86`) | per RPC frame in each direction | ~144k | 42.5% | 4.0% | ~7k | derived (0.878 × outbound) | |
| Close and other events | `webSocketClose` `:321` | | per session | ~61k | 18.0% | 1.7% | ~3k | derived | |
| `GET /status` relay probe, plus `POST /nudge`, sidecar slots, sessions closed cleanly | `:201`, `:215`, `:179` | `relay_probe_task` `crates/engine/src/host/workspace_host/presence.rs:305`: 30 s sweep (`:18`) over devices whose presence is stale; backoff to 300 s while it answers alive (`:43`), to 1,800 s offline (`:30`); wake, foreground and registry reconnect make all due at most every 300 s (`:38`). Nudge: `crates/engine/src/host/doc_host/commands.rs:843`, `apps/ios/Cypher/App/AppConfig.swift:329`, BotRoom | 48 a day per (engine, long-offline device row), 288 a day per (engine, alive but presence-stale device) | ~61k | 17.8% | 1.7% | ~61k | derived (floor 1,952 a day; Nov 15 had 2,370 requests and 7 outbound messages) | scales with engines × stale device rows in the registry |

### BotRoom: 36,850 analytics, 36,850 billed

Edge: `apps/edge/src/bots/bot-room.ts` (HTTP only), routes `apps/edge/src/room-routes.ts:113-128`.

| Source | Edge | Client | Trigger and frequency | Requests | Class share | Total share | Billed | Confidence | Notes for reduction |
|---|---|---|---|---:|---:|---:|---:|---|---|
| Inbox poll `GET /bots/:id/inbox` | `fetch` `:330` (runs `sweep` `:248` first), `pull` | `PULL_EVERY` 60 s `crates/engine/src/bots/network/host.rs:26`; loop `:35-57`; a bot is polled while it has a live connection or an unsettled outgoing exchange (`:86-98`); `crates/engine/src/bots/network/edge.rs:167` | 1,440 a day per connected bot on its host engine, for as long as the connection exists | ~36.5k | 99% | 1.0% | ~36.5k | derived (Nov 23: 4,880 = 3.4 bots × 1,440) | 4.9k a day at the end of the window = 146k a month; the room already nudges the host's DeviceRoom when an entry is queued (`nudgeHost` `:297`) |
| `publish`, `ask`, `answer`, `state`, `ack`, invites, connect | `owner` `:360` | `edge.rs:116` | per bot action | <300 | | | <300 | guessed | |
| DO-to-DO: peer room `/peer/*`, host DeviceRoom `/nudge` | `peerCall` `:317`, `nudgeHost` `:297` | | per ask, answer, state | 80 sent | | | 80 | measured (subrequests) | each lands as one request on the other room |
| Alarm | `setAlarm` `:286`, `alarm` `:290` | | exchange expiry (7 days) | ~0 | | | | guessed | |

### PushDevice: 17,260 analytics, 17,260 billed

Edge: `apps/edge/src/notifications/push-device.ts` (HTTP only, reached only DO-to-DO and
from the revoke route).

| Source | Edge | Caller | Trigger and frequency | Requests | Confidence | Notes for reduction |
|---|---|---|---|---:|---|---|
| `/send` (alert or badge) | `:80-131`; then the APNs call through the `APNS_SENDER` service binding (`:113`) | RegistryRoom alarm: `deviceCall` `notifications.ts:634`, alerts `:701`, badge to every registration `:734` | per notice and per badge change, per phone | ≤9.4k, 7,551 reached APNs | measured (subrequests of both classes) | every send lists up to 512 delivery keys (`:127`): 120 rows read a request |
| `/register` | `:41-68` | RegistryRoom `register` `notifications.ts:475-490`, from iOS at bind and every foreground | per iOS foreground | within the 9.4k | derived | |
| `/unregister` | `:73-78` | RegistryRoom `:512`; Worker `apps/edge/src/revoke-route.ts:22` from iOS `drainRevocations` (`NotificationController.swift:363`), retried every foreground until a 2xx | per logout or token change | unknown | guessed | |
| Not accounted for | | | PushDevice received 17,260 requests while RegistryRoom sent 9,628 subrequests | ~7.6k | unexplained | 82% of the class is one object (one phone) |

### Billed estimate

| Class | Analytics | 1:1 (HTTP, upgrade, alarm) | WebSocket messages | Billed | Share of billed |
|---|---:|---:|---:|---:|---:|
| TrustRoom | 352,030 | 352,030 | 0 | 352k | 36% |
| RegistryRoom | 2,250,890 | ~196k | ~2,055k | ~299k | 30% |
| DeviceRoom | 339,670 | ~134k | ~206k | ~144k | 15% |
| ChatRoom | 619,440 | ~114k | ~505k | ~139k | 14% |
| BotRoom | 36,850 | 36,850 | 0 | 37k | 4% |
| PushDevice | 17,260 | 17,260 | 0 | 17k | 2% |
| **Total** | 3,616,140 | ~850k | ~2,766k | **~0.99M** | |

Bounds from the Worker count alone: if RegistryRoom and ChatRoom had no HTTP beyond the
upgrades their statuses show, billed is 0.85M; if every unattributed Worker request were a
DO request, 1.35M.

Run rate in the last week (Nov 17–23, no incident), billed per day: TrustRoom 14,886,
RegistryRoom ~6,950, ChatRoom ~5,940, DeviceRoom ~4,110, BotRoom 3,800 (4,880 on Nov 23),
PushDevice 624: **~36,300 a day, 1.09M per 30 days**. One always-on engine costs about
43,200 TrustRoom polls and 172,800 presence beats (8,640 billed) a month before it does
anything.

## Facts the designs rely on

Sockets:

| Room | Accept | Keepalive | Liveness the client relies on |
|---|---|---|---|
| RegistryRoom | `ctx.acceptWebSocket` (hibernatable) | text `ping` every 15 s, auto-answered | silence lease 45 s; `probe` frame after 900 s quiet, 10 s deadline |
| ChatRoom | hibernatable | same | same, plus a 30 s push-ack deadline |
| DeviceRoom | hibernatable, tagged host / client | host and Rust client every 15 s, iOS client every 30 s; the DO reads the auto-response timestamp for host liveness (75 s) | silence lease 40 s; iOS probes with `ListHarnesses` |
| TrustRoom, BotRoom, PushDevice | no sockets | | |

No `ws.accept()`, no `setTimeout` or `setInterval`, no RPC methods in `apps/edge/src`.

Heartbeats and polls:

| What | Interval | Transport | Per |
|---|---|---|---|
| Registry presence beat | 15 s | WS message | joined client |
| Session freshness touch (registry push) | 20–30 s | WS message + ~3.7 rows written + broadcast | live run on its host |
| Trust chain resync | 60 s | HTTP | running engine, foregrounded phone |
| Bot inbox poll | 60 s | HTTP | connected bot on its host |
| Relay status probe | 30 s sweep, backoff to 300 s / 1,800 s | HTTP | (engine, stale device) |
| Quiet probe | 900 s | WS message | registry or chat socket |
| iOS HTTPS fallback poll | 20 s while not joined, once at start | HTTP | registry and each of ≤8 live chats |
| Pairing mailbox poll | 2 s while pairing | HTTP | pairing in progress |
| Notification event retry for an ignored chat | 30 s → 600 s | HTTP | chat and status |
| Token refresh, update check (Worker only, no DO) | per token lifetime; 60 min (`crates/update/src/lib.rs:47`) | HTTP | engine |

Hops:

| Hop | Where | Count per action |
|---|---|---|
| Worker → DO | `forward` `apps/edge/src/router.ts:29-44`; every `/chat2`, `/registry`, `/device`, `/bots`, `/trust` request and upgrade | 1 |
| RegistryRoom → PushDevice | `notifications.ts:490` (register), `:636` (send, unregister) | 1 per phone per alert, 1 per phone per badge change, 1 per iOS foreground |
| BotRoom → BotRoom, BotRoom → DeviceRoom | `bot-room.ts:317`, `:297` | 1 each per ask, answer, state |
| Worker → PushDevice | `revoke-route.ts:22` | 1 per revoke attempt |
| PushDevice → `APNsSender` (service binding, a Worker invocation) | `push-device.ts:113` | 1 per send |

Alarms: RegistryRoom (notification outbox and badge, daily backup and GC), ChatRoom (daily
backup while dirty), BotRoom (exchange expiry), SessionRoom (no-op).

Observations, not designs:

- The three fixed HTTP polls (trust 60 s, bot inbox 60 s, relay status) are 1:1 and run
  whether or not anything changed; each client that makes them already holds an open
  RegistryRoom socket, and each engine an open DeviceRoom host socket.
- Every socket (re)connect pays an upgrade and an HTTPS pull for the same rows.
- Moving the presence beat changes the analytics count by 44% and the bill by 8%.
- Billed traffic sits at the 1M line. If overage is charged in whole millions, the next
  $0.15 is saved by any cut of about 0.1M and lost again by one incident week or by two or
  three more always-on engines; if it is pro rata, each 100k is worth $0.015.

### Corrections to the attribution

Changes made to the step 1 text in step 2; everything else above is as step 1 wrote it.

| Where | Step 1 said | Now |
|---|---|---|
| First bullet, the billing rules table, the last observation, the "Assumed" list, the brief table | Overage is rounded up to the next million, so $0.15 means 1–2M billed | Not confirmed: a second lookup of the pricing page found a prorated worked example. Both readings are carried; the invoice line settles it (M1) |
| Billing rules, auto-response row | "assumed unbilled", with the page's wording as a caveat | "not stated; assumed unbilled": the page exempts protocol pings by name and speaks of auto-responses only under duration. A row for protocol pings added |
| Step changes, Nov 21–23 | unexplained | cause still not established; an inference added (one client at the dial cooldown cap) |
| Section titles | "Step 1: attribution", "Facts for step 2" | "Where the requests come from", "Facts the designs rely on" |

Found in step 2 and not in the tables above (they are in the option designs): the bot
inbox poll is a fallback beside a nudge that already exists, and progress states are not
nudged (B1); a host re-reports a hidden chat three times per transition (N1); an unknown
frame type makes both registry clients redial (common ground of the designs).

## Verified vs inferred

Verified (read from the snapshot or the code):

- Per-class, per-day, per-status and per-object counts, periodic statistics, Worker counts.
- All sockets hibernatable with auto-response; every interval and constant cited above.
- `inboundWebsocketMsgCount` is 0 everywhere; analytics requests include inbound messages
  and exclude auto-answered pings (the RegistryRoom fit; DeviceRoom's quiet days).
- TrustRoom, BotRoom and PushDevice are HTTP only, so their analytics count is their billed
  count.

Inferred (fits and arithmetic that reproduce the snapshot):

- The disconnect statuses mark one WebSocket session each; so the upgrade counts are lower
  bounds.
- The presence, write-driven and per-reconnect buckets of RegistryRoom, ChatRoom and
  DeviceRoom, and the engine-equivalent counts.
- Billed ≈ 0.99M, and that the $5.15 invoice covers a cycle like this window.
- The Nov 6–11 cause beyond the measured dial loop (the Linux restart loop's share).

Assumed:

- The billing rules as a search tool summarised the pricing page on 2026-11-24, not the
  page text; they agree with `docs/operations/cloudflare-billing.md` except on overage
  rounding, where two lookups disagree.
- Auto-answered text `ping` frames are not billed as inbound messages. Every socket sends
  one every 15 s (iOS relay 30 s): at 4–14 sockets per engine-equivalent that is 6–22M
  frames a month, which at 20:1 would be 0.3–1.1M billed requests, more than any line
  above. The invoice does not exclude it: $0.15 is 1–2M billed if charged per whole
  million and 2.0M if pro rata, and 2.0M is this estimate plus 1.0M of pings (116 open
  sockets, 11 per engine-equivalent).

Guessed (not checkable from the snapshot):

- RegistryRoom notification HTTP (~75k) and alarms (~10k); ChatRoom checkpoints and alarms.
- Whether close events bill at 20:1 (assumed) or not at all (difference under 10k).

Open, with the measurement that settles each:

| Question | Measurement |
|---|---|
| HTTP / alarm / hibernation per class and day | the snapshot query with `dimensions{date namespaceId type}` (as `scripts/ops/cf-usage.py:103`) |
| RegistryRoom and ChatRoom HTTP by path; non-DO Worker requests by path | `scripts/ops/edge-billing-prod.py --seconds 305` on a working day |
| DeviceRoom: who dials, and the Nov 21–23 churn | `dimensions{date objectId}` filtered to the DeviceRoom namespace; count `peer: dialing via device room` in engine logs |
| Oct 26–30 | the same per-object query for that week; engine logs of the Linux device |
| PushDevice's ~7.6k extra requests | tail sample filtered to `PushDevice`, path `/unregister` |
| DeviceRoom duration (180,735 s) | `activeTime` by `objectId` and day |
| Which billing cycle the $5.15 invoice covers | `scripts/ops/cf-usage.py` (reads the cycle from the subscription) |
| Whether auto-answered `ping` frames bill at 20:1 | the invoice's billed Durable Object request quantity for one cycle against `http + alarm + hibernation / 20` from `scripts/ops/cf-usage.py` for the same cycle |

### Step 2

Verified (read in the code at `53c34485`):

- Every file and line a design cites, and the compatibility facts in the common-ground
  table: unknown frame types redial, unknown fields are ignored, unknown relay kinds are
  dropped, `hello` is read field by field.
- B1's premises: the nudge and its call sites, the DeviceRoom's queue and replay, the 75 s
  liveness window, the missing nudge on `state`, the discarded nudge answer.
- C1's premises: the pull started beside every dial in the Rust actors and at `start` on
  iOS. N1's: three attempts per first ignore of a signature, the `hidden` reason and the
  Edge's statement that its markers do not change. G1's: no healthy-session rule and no
  jitter in the registry and chat actors, both present in the host relay.
- The Worker already answers 401, the registry's 403 and unknown paths without an object.
- No scheduled job runs `scripts/ops/cf-usage.py` (no `schedule` in `.github/workflows`).

Inferred (arithmetic on the snapshot and the constants in the code):

- Every saving: run rate of Nov 17–23 × 30 days, less what the design still sends.
- The fixed-poll share (65%) and the 10× and 100× projections, which scale every term
  linearly with devices, bots and use.
- The Nov 21–23 reading (one caller at the cooldown cap) and the incidents' billed excess.
- P1's 46%: engine-equivalents per registry from each object's share of requests.

Guessed:

- N1 (0–40k) and N2 (up to 55k): both sit inside step 1's guessed notification HTTP line.
- D2 (up to 25k): the status probe's part of DeviceRoom's HTTP floor is unknown.
- S1: that the freshness touch is half of the write-driven RegistryRoom bucket.
- The latency cost of C1 on a healthy network (0.1–0.2 s): reasoned from connection reuse,
  not measured.

Not done:

- Nothing was built, run, tested or deployed; no option's saving is measured.
- No Cloudflare API call, no invoice read: M1–M5 are all open.
- Cloudflare's documentation was read only through a search tool's summaries: the 20:1
  footnote, the prorated example, protocol pings being free and answered without a wake.
- Not read: the desktop UI's staleness gate (S1), the iOS chat and relay clients beyond the
  cited lines, how an old client treats a presence echo of its own device (P1).

## Where the brief's numbers did not hold

| Brief | Found |
|---|---|
| "Last period ≈ 3.6M DO requests" against "1M included, then $0.15 per million" | 3,616,140 is the analytics count, which matches. Billed is about 0.99M: inbound WebSocket messages bill 20:1 and are about 2.77M of the 3.62M. |
| "Bill $5.15" as the cost of 3.6M | 2.6M of overage costs $0.39 pro rata or $0.45 in whole millions. $0.15 is billed requests between 1M and 2M (whole millions) or 2.0M (pro rata). |
| RegistryRoom 62% is the bill's main line | 62% of the analytics count, about 30% of billed; TrustRoom (10% of the count) is about 36%. |
| "Workers requests 1.2M (November ≈ 2× October)" | 1,198,260 for `cypher-edge` (1,205,350 with `cypher-www-redirect`). The window holds 8 October days; per day November is 1.4× October, and all of it is Nov 6–11 (41% of the window). Without those days November is below October. |
| `inboundWebsocketMsgCount` can be reconciled with requests | It is 0 for every class and day; only `outboundWebsocketMsgCount` is populated. |
| Classes may use `ws.accept()`, RPC | None does: all hibernatable, all `stub.fetch`. |
| Release and update checks may hit a DO | They do not: `/releases/*` is served by the Worker from R2 (`apps/edge/src/public-routes.ts:31`). |
| SessionRoom among the active classes | Bound, 410 stub, zero requests. |

The per-class totals in the brief (2.25M, 0.62M, 0.35M, 0.34M, 37k, 17k) match the snapshot.

From the step 2 brief:

| Brief | Found |
|---|---|
| "$5.15 = $5 base + one $0.15 overage unit" | Holds only if overage is charged per whole million. A second lookup found a prorated example on the pricing page; pro rata, $0.15 is 2.0M billed, 1.0M more than the model. Open (M1). |
| Presence from ping/pong auto-response is "free" | Free of duration and of wakes. Whether an auto-answered text `ping` is free as a request is exactly the open question; the page exempts protocol pings by name. P1 depends on it. |
| "Worker-only first since it deploys on merge" | The order holds, the saving does not: no Worker-only change removes a large line, because the polls are client-initiated and need the object's answer. PD1 (up to 3k) is the only Worker-only saving. |
| Worker-level short-circuit for 401/404 and unchanged reads | 401, the registry's 403 and unknown paths never reach an object today. A cache for unchanged reads cannot be invalidated across data centres and would delay removals, pairings and bot messages. |
| Replace the bot inbox poll with push | The push exists (BotRoom nudges the host's DeviceRoom); the poll is its fallback. The work is making the nudge acknowledged. |
| "the exact `scripts/ops/cf-usage.py` invocation with the `type` dimension and per-namespace filter" | The script has no such options: it sums one billing cycle for the account and prints the split under `detail.do_invocations_raw` with `--json`. The per-day, per-namespace query is given under Phase 0. |
| Today's dollar saving is small | It is at most the $0.15 on the invoice; at 10× about $0.94 a month; the DO meter that costs real money at scale is rows written. |

## Decisions for the owner

| # | Decision | Recommendation |
|---|---|---|
| 1 | Read the invoice line for Durable Objects requests (M1) and run M2–M4 with a token | Do first: 10 minutes, settles the rounding rule, the `ping` question and the three guessed lines |
| 2 | Whether to do phases 1–2 at all, given a saving of at most $0.15 a month today | Yes for T1, B1 and the guards: they are what keeps the bill flat as engines and bots are added and what bounds a bad release. C1, D1, N1 ride the same release at small cost |
| 3 | T1's safety poll: 5, 15 or 30 min | 15 min. It is the worst case for a removal to reach a device when the push and its retry both fail; the normal case becomes about 1 s |
| 4 | C1: accept first rows up to 0.1–0.2 s later on a cold open, 1.5 s later where WebSockets are blocked | Yes, after the staged timing check |
| 5 | D1: a 10 min dial cooldown for a device known to be offline | Decide after M4 names the caller; if it is a background task, stop the caller instead |
| 6 | D2: let the status probe of a device gone for over a week back off to 6 h | Only if M3 shows the probes are a real line |
| 7 | P1 and S1, which move the dashboard number and little of the bill | Not now. P1 only after M1; S1 when rows written approach their inclusion (about 20× today's use) |
| 8 | G2: a daily scheduled usage guard, which needs a read-only Cloudflare token as a repository secret | Yes |
| 9 | If M1 shows pings are billed: K1 first, and how the DeviceRoom then learns host liveness | Keep text pings on the host socket only |
| 10 | Housekeeping outside this document: an index entry in `docs/README.md`; rule 2 of `docs/operations/cloudflare-billing.md` ("`ping` costs nothing") if M1 contradicts it; a per-day, per-namespace option for `scripts/ops/cf-usage.py` | With phase 1 |
