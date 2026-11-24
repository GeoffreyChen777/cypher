# Durable Object cost

Where the Edge's Durable Object (DO) requests come from, per class and per code path, so
they can be cut without changing what the app does. Measurement tools and the earlier
cost rules are in [Cloudflare billing and measurement](../operations/cloudflare-billing.md).

## Step 1: attribution

Source: one usage snapshot, 2026-10-24..2026-11-23 (31 days), and the code at `53c34485`.
Nothing here was measured live; the split inside each class is a model fitted to the
snapshot ([how](#how-the-classes-were-split)).

- **3,616,140 is the analytics count, not the billed count.** Analytics count every
  inbound WebSocket message as one request; billing folds them 20:1. Billed requests for
  the window are **about 0.99M (plausible 0.9–1.2M)** against 1M included. A $5.15 invoice
  is $5 plus exactly one $0.15 unit, which agrees: overage is rounded up to the next
  million, and 3.6M billed would have cost $5.45. One assumption carries this figure:
  that auto-answered `ping` frames are not billed ([open](#verified-vs-inferred)).
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
(looked up 2026-11-24 through a search tool that returns a summary, not the page text) and
this repository's own measurements. The rounding rule and the auto-response row are the two
to re-read on the page itself.

| Item | Billed as | Source |
|---|---|---|
| `fetch()` to a DO stub (from the Worker or from another DO) | 1 request | pricing page |
| RPC method call on a stub | 1 request per RPC session | pricing page; the Edge uses none, every call is `stub.fetch` |
| WebSocket upgrade | 1 request | pricing page |
| Incoming WebSocket message | 1/20 request | pricing page, footnote on the 20:1 ratio |
| Outgoing WebSocket message | free | pricing page |
| `ping` answered by `setWebSocketAutoResponse` | no wake and no duration (pricing page). As a request: **assumed unbilled**. It produces no invocation (`docs/operations/cloudflare-billing.md`: 20 pings, 0 spans) and is absent from analytics, but the page exempts "protocol pings" by name and a text `ping` frame is not one | to confirm against an invoice |
| Alarm invocation | 1 request; `setAlarm` also bills one row written | pricing page |
| Included / overage | 1M requests a month, then $0.15 per million, **rounded up to the next million** | pricing page |
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
| Nov 21–23 | DeviceRoom 9.9k / 21.4k / 11.4k with 2.4k / 6.3k / 3.2k dropped sessions, RegistryRoom drops unchanged (~300) | `634ffa02` (Nov 21 15:21: a host that rejoins sends every client `host_closed`), 0.4.7, 0.4.8 | **unexplained**; device-room only |

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

## Facts for step 2

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
- Billed traffic sits at the 1M line, and overage is charged in whole millions: the next
  $0.15 is saved by any cut of about 0.1M, and lost again by one incident week or by two
  or three more always-on engines.

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
  page text; they agree with `docs/operations/cloudflare-billing.md`.
- Auto-answered text `ping` frames are not billed as inbound messages. Every socket sends
  one every 15 s (iOS relay 30 s): at 4–14 sockets per engine-equivalent that is 6–22M
  frames a month, which at 20:1 would be 0.3–1.1M billed requests, more than any line
  above. The invoice (one unit, so 1–2M billed) does not exclude it.

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

## Where the brief's numbers did not hold

| Brief | Found |
|---|---|
| "Last period ≈ 3.6M DO requests" against "1M included, then $0.15 per million" | 3,616,140 is the analytics count, which matches. Billed is about 0.99M: inbound WebSocket messages bill 20:1 and are about 2.77M of the 3.62M. |
| "Bill $5.15" as the cost of 3.6M | 2.6M of overage is 3 billing units, $0.45. $5.15 is one unit: billed requests of that cycle were between 1M and 2M. |
| RegistryRoom 62% is the bill's main line | 62% of the analytics count, about 30% of billed; TrustRoom (10% of the count) is about 36%. |
| "Workers requests 1.2M (November ≈ 2× October)" | 1,198,260 for `cypher-edge` (1,205,350 with `cypher-www-redirect`). The window holds 8 October days; per day November is 1.4× October, and all of it is Nov 6–11 (41% of the window). Without those days November is below October. |
| `inboundWebsocketMsgCount` can be reconciled with requests | It is 0 for every class and day; only `outboundWebsocketMsgCount` is populated. |
| Classes may use `ws.accept()`, RPC | None does: all hibernatable, all `stub.fetch`. |
| Release and update checks may hit a DO | They do not: `/releases/*` is served by the Worker from R2 (`apps/edge/src/public-routes.ts:31`). |
| SessionRoom among the active classes | Bound, 410 stub, zero requests. |

The per-class totals in the brief (2.25M, 0.62M, 0.35M, 0.34M, 37k, 17k) match the snapshot.
