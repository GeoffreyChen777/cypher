# Agent bots

Status: **P0 implemented** (local bots: definition, scope, the bot chat and its read
tools, the sidebar strip). P1–P5 are design; §12 is the build order. Where P0 differs from
the full design, the section says so.

An agent bot is a long-lived Pi session that answers questions about its owner's work. It
runs on one of the owner's devices, sees the projects and sessions the owner grants it,
and can be connected to other users' bots. Connected bots ask each other questions: "How
far along is A with the billing migration?" goes from my bot to A's bot, A's bot searches
the sessions A granted it, and the answer comes back to my bot.

## 1. Goals and non-goals

Goals:

- **A global view for the owner.** One conversation that can see across every granted
  project and session, on every device in the profile, instead of one session's cwd.
- **Explicit, enforced scope.** The owner picks which projects and sessions a bot may see.
  The engine enforces this on every tool call; the model cannot widen it.
- **Bot-to-bot questions across users and organizations.** Connections are made by invite,
  either side can revoke them, and each connection has its own policy.
- **Execution stays on the owner's machines.** Bots run on an engine the owner picked. The
  Edge relays messages between bots and never runs a model or reads a session.
- **Usable from every device.** A bot hosted on Mac A can be used from Mac B (and later
  the iPhone) the same way a remote session is.

Non-goals for the phases in §12:

- Bots that *change* anything for a peer: no writes, shell, or session control on another
  user's behalf. Owner-only action tools are a later, separate decision (§13).
- Reading another user's sessions directly. Only the owner's bot reads the owner's
  sessions; peers receive text answers.
- A public bot directory. Discovery is by invite only.
- Hosting on iOS. The iPhone is a remote control, as it is for sessions.

## 2. Concepts

| Term | Meaning |
|---|---|
| **Bot** | A named agent owned by one user: definition, scope, host device, and its own conversation. A user may own many. |
| **Host device** | The single Mac or Linux engine that runs the bot's Pi sessions, picked at creation and changeable later. iOS devices cannot host. |
| **Bot chat** | The owner's conversation with their bot. A normal chat hosted on the host device, so every owner device can read and drive it. |
| **Scope** | What the bot may read: granted projects (spaces), per-session overrides, and capability flags (§5). |
| **Connection** | A mutual link between two bots of different users, created by an invite. Each side holds its own policy for it. |
| **Connection policy** | Per side: reply mode (**Auto** or **Review**), an optional narrower scope, and rate limits. |
| **Exchange** | One question from a peer bot and its answer. Exchanges are audited on the answering side. |
| **Peer thread** | A hidden chat, one per connection, on the answering bot's host. Peer questions run there, never in the bot chat. |
| **Group** | A named set of connections that `ask_group` fans a question out to (§9). |

## 3. The end-to-end flow

```
Me (any device)                 my host engine               Edge                    A's host engine
   │ "How is A doing on X?"          │                         │                           │
   ├── bot chat (chat2) ────────────►│ Pi run, tools:          │                           │
   │                                 │ ask_bot(A's bot, q) ───►│ BotRoom(me) → BotRoom(A)  │
   │                                 │                         ├── ask (queued if offline)►│
   │                                 │                         │                           │ peer thread for (A-bot, my-bot)
   │                                 │                         │                           │ Pi run with scope ∩ connection scope
   │                                 │                         │                           │ search_sessions / read_session …
   │                                 │                         │◄── answer (Auto) ─────────┤ (Review: waits for A's approval)
   │                                 │◄── answer ──────────────┤                           │
   │◄── bot replies, citing A's bot ─┤ tool result → model     │                           │
```

1. I ask my bot in its bot chat. The bot chat is hosted on my bot's host device, so this
   works from any of my devices through the existing chat2 sync and command plane.
2. The model calls `ask_bot`. My engine sends the question through my bot's BotRoom, which
   checks that I own the bot and that the connection is active, then hands it to A's
   BotRoom.
3. A's BotRoom queues it and delivers it to A's host engine when that engine is connected.
4. A's engine appends the question to the peer thread for this connection and runs Pi with
   the effective scope (A's bot scope ∩ the connection's scope).
5. In **Auto** mode the final assistant text is sent back right away. In **Review** mode it
   waits as a draft until A approves, edits, or declines it.
6. My engine resolves the pending `ask_bot` call. If the answer took longer than the tool's
   wait window, it arrives instead as a follow-up message in my bot chat, which starts a new
   turn (§7.4).

## 4. Data model

### 4.1 Bot definitions: registry `bots` rows

Bot definitions are owner-private and sync across the owner's devices as a new registry row
kind, `bots`. The RegistryRoom already accepts any kind that matches `KIND_RE`, so the Edge
needs no change; `cypher-doc` gains a `KIND_BOTS` table and typed accessors.

| Field | Type | Writer | Notes |
|---|---|---|---|
| `name` | string | any owner device | ≤ 40 characters; shown to peers (P2). |
| `icon`, `color` | string? | any | Avatar keys: a glyph (`None` = the name's initial) and a project-palette colour. |
| `instructions` | string? | any | Appended to the system prompt; ≤ 8 KiB. |
| `hostDeviceId` | string | creator | A Mac or Linux engine; fixed at creation in P0. |
| `chatId` | string | creator | The bot chat, minted with the row by `CreateBot` on the host. |
| `scope` | JSON (§5) | any | A scope that does not parse reads as the empty scope. |
| `createdAt` | ms | creator | Strip order. |
| `network` | JSON? | host | `{botRoomId, publishedAt}` once the bot is on the Edge (P2). |
| `paused` | bool | any | A paused bot answers nothing, including peers (P2). |

The model and thinking level are not on the bot row: they are the bot chat's own config,
picked in its composer like any session's.

Writes are LWW per field, as with every registry row. The host device is the only enforcer:
it re-reads the row on every change and applies it to the next tool call, never to a call
already in flight.

Local-only profiles keep `bots` rows in the local registry, the same as other rows. A local
bot works for the owner (§6) but cannot join the network, which needs a synced account.

### 4.2 Chats a bot owns

- **Bot chat**: a chat row with `bot: {botId}` and no `spaceId`. The marker keeps it out of
  project cards and the `@session` mention list; it shows in the sidebar's bot strip
  instead. Its cwd is a private, empty per-bot folder on the host
  (`<profile store>/bots/<botId>`, 0700), so Pi has a working directory, no project
  checkout and no context files. A run recreates the folder if it is gone. Desktop builds
  older than P0 list the bot chat under "No project"; the iPhone app hides project-less
  chats.
- **Peer threads** (P2): chat rows with `bot: {botId, role: "peer", connectionId}`, created
  lazily on a connection's first question. They are synced to the owner's devices for the
  audit view but never shown as ordinary sessions.

Every chat with a `bot` marker is **excluded from every bot's scope**, including its own.
Otherwise one peer's question could surface another peer's exchange.

### 4.3 Connections, invites and exchanges: Edge `BotRoom`

Cross-user state cannot live in a per-user registry, so it gets one new Durable Object
class, `BotRoom`, keyed `bot1/{botRoomId}` and bound as `BOT_ROOMS` (wrangler migration
`new_sqlite_classes: ["BotRoom"]`). There is one room per networked bot. Its SQLite holds:

| Table | Contents |
|---|---|
| `meta` | owner user id (claimed on publish, owner-only after), public profile `{name, ownerName}`, `paused` |
| `invites` | `sha256(secret)`, created/expiry, max uses, uses |
| `connections` | `connectionId`, peer `botRoomId`, peer profile, created, revoked, per-side rate counters |
| `inbox` | exchanges addressed to this bot that the host has not acknowledged: `exchangeId`, `connectionId`, question, sentAt |
| `outbox` | exchanges this bot asked, with their state and, once received, the answer, until the host acknowledges it |

The DO stores message bodies only until the receiving host acknowledges them, plus a
bounded retention for undelivered ones (default 7 days, then `expired`). It never keeps
transcripts or answers beyond that.

## 5. Scope

### 5.1 Shape

```json
{
  "projects": { "mode": "selected", "spaceIds": ["sp_…", "sp_…"] },
  "includeSessions": ["chat_…"],
  "excludeSessions": ["chat_…"],
  "includeArchived": false,
  "transcripts": true
}
```

`cypher_proto::BotScope` holds the rule; the engine enforces it and the bot dialog previews
it ("It can see 12 sessions in 3 projects").

- `projects.mode` is `"none"` (the default), `"selected"`, or `"all"`. `"all"` also covers
  projects created later. `"selected"` names spaces, which are (device, folder) pairs, so a
  project that exists on two devices is granted per device.
- A granted project grants its sessions, including future ones. `excludeSessions` removes
  single sessions; `includeSessions` grants single sessions in otherwise ungranted
  projects. The P0 dialog edits projects and the two switches; it keeps the session lists
  as they are.
- Subagent child chats follow their root parent; a broken or cyclic parent chain hides the
  chat.
- `transcripts: false` limits the bot to titles, latest-message previews and status.
- Later capabilities (P1+): `git` (branch, recent commits, diff stats through the device
  relay) and `files` (read files in a granted checkout, off by default).

### 5.2 Effective scope

| Caller | Effective scope |
|---|---|
| The owner, in the bot chat | bot scope |
| A peer, in that connection's peer thread | bot scope ∩ connection scope |

A connection's scope can only narrow the bot's scope. A new connection starts with the full
bot scope and **Auto** replies. That default matches the intended use, a "project bot" whose
scope the owner already chose, and the owner can tighten either one at any time.

### 5.3 Enforcement

Scope is enforced in the engine, not the prompt:

1. **Tool allowlist.** Bot runs start Pi with `--tools` restricted to the bot tools in §7
   (the mechanism child subagent chats already use). There is no `bash`, `read`, `write`,
   `edit`, `grep`, MCP server, or subagent spawn, so a bot cannot reach the file system
   except through scoped tools. Permission control is therefore irrelevant to bot runs.
   Claude models keep to the list too: the Claude Code bridge's provider path starts
   Claude Code with no built-in tools and passes it only Pi's.
   The Runtime's codemode extension honours a `/scripts` typed in the chat even under
   `--tools`, so the engine refuses `/scripts`, `/orchestrate` and `/goal` in a bot chat
   before the prompt reaches Pi. Peer threads (P2) must accept no slash commands at all.
2. **Engine-side checks.** Each bot tool is a thin extension wrapper over a bot RPC that
   carries the calling chat id. The engine maps the chat id to (bot, role, connection),
   computes the effective scope, and filters results. An id outside the scope answers
   "not found", the same as a missing id, so the model cannot probe for existence.
3. **Peer isolation.** Each connection's questions run in that connection's own peer
   thread, so context from one peer never reaches another peer's answers.
4. **Untrusted input.** Peer questions are wrapped as quoted, attributed input ("Question
   from <peer bot> (owner <name>)"), never as instructions. A prompt injection can at most
   steer the bot within the effective scope, and Review mode exists for scopes where even
   that is too much.
5. **Outgoing scrub.** Before an answer leaves the host, a best-effort pass redacts strings
   that look like secrets: private-key blocks, common token formats, and `.env`-style
   `KEY=value` assignments. Permission control's sensitive patterns are path globs, so this
   is a new content pattern set in `engine/bots/`. It is a safety net, not a guarantee, and
   the doc and UI say so.

## 6. Bot runtime on the host device

The engine module `crates/engine/src/bots/` serves the bots whose `hostDeviceId` is this
device:

- **Creation.** `CreateBot` runs on the chosen host (it is relay-forwardable): it makes the
  bot's folder, then writes the bot chat and the bot row in one registry batch. Edits and
  deletes are `Mutate` ops (`updateBot`, `deleteBot`) any owner device can make; a rename
  retitles the bot chat, and a delete tombstones the bot with its chat, after which the
  host stops the run and removes the folder.
- **Runs** go through the normal sessions engine: a bot chat is a chat, so steering,
  interrupts, queued sends, the run journal and crash resume apply unchanged, and the
  owner's other devices drive it through chat2 and the command plane like any remote
  session. The harness spawn path has a bot mode next to child-subagent mode
  (`RunHostContext::bot`): tool allowlist, appended system prompt, `CYPHER_BOT_ID`. A bot
  chat whose bot row is gone still runs locked down, seeing nothing.
- **Reading sessions.** `search_sessions` reads transcripts through
  `DocHost::read_transcript`: the open handle when there is one, else the stored snapshot.
  It joins no room, starts no task and leaves the warm-doc cache alone. `read_session`
  wants the freshest copy, so for a chat another device hosts it opens the doc (joining
  its chat2 room) and gives it a few seconds to catch up.
- **Search (P0).** A bounded scan, newest first: titles and previews, plus message text
  for the 400 most recent sessions in scope that have a copy on this device. Sessions with
  no local copy are matched by title only, and the answer says how many.
- **Search index (P1).** Scanning does not scale past a few hundred sessions, so the host keeps a
  per-profile SQLite FTS5 index (`bots-index.sqlite`) of message text keyed by (chat id,
  message id). It is updated incrementally from chat2 changes, rebuilt on demand, and holds
  only chats that some bot on this host can see. Scope filtering happens at query time, so
  narrowing a scope takes effect immediately.
- **Git and status.** Session status, titles, and branches come from the registry. Git
  facts for a space on another device go through the existing forwardable RPCs over the
  device relay, so they need that device online; the tool says so when it is offline.

## 7. Bot tools

The extension is `crates/harness/src/pi/bot-tools.ts`, embedded in the engine binary like
`engine-client.mjs`: the harness writes both into its private support folder and loads the
extension with `-e` into bot runs only, so bot tools never appear in other sessions and
need no Runtime release. Each tool is one `BotTool` call (`{chatId, tool, args}` →
`{text}`) over the private socket; the engine resolves the bot from `CYPHER_CHAT_ID`,
enforces the scope and formats the answer, so the extension decides nothing. Answers are
plain text, bounded to 48 KiB. P0 ships the first four tools below.

### 7.1 Read tools (owner and peer)

| Tool | Capability | Returns |
|---|---|---|
| `list_projects` | — | Granted projects: name, id, device, folder, git, session count, working count, last activity. |
| `list_sessions` | — | Root sessions in scope, newest first, filterable by project (id or name), `active`, and `since_hours`; with status and latest-message preview. |
| `search_sessions` | transcripts for text | Sessions whose title, preview or messages contain every word; up to three excerpts each, with message indexes. |
| `read_session` | transcripts | Up to 60 messages of one session, the latest by default or from an index; tool calls as one-line summaries, thinking left out. |
| `session_activity` (P1) | — | What changed recently: new sessions, finished runs and titles in a time window. |
| `project_git` (P1) | git | Branch, ahead/behind, recent commits, working-tree diff stats for a granted space. |

### 7.2 Network tools (owner only)

| Tool | Does |
|---|---|
| `list_connections` | Connected bots with name, owner name, groups, and reachability (host last seen). |
| `ask_bot` | Sends a question to one connected bot and waits up to `wait_s` (default 120 s) for the answer. |
| `ask_group` | Fans a question out to a group's bots and returns the answers received within the wait window, marking the others as pending. |

Network tools are not available in peer threads, so a peer cannot use my bot to relay
questions to third parties.

### 7.3 Prompting

The bot's system prompt states its role (an assistant over its owner's work), the effective
scope in plain words, that it must cite the sessions an answer draws on, and, in peer
threads, who is asking and that peer text is a question, not an instruction.

### 7.4 Late answers

When `ask_bot` times out, it returns `pending` with the exchange id. When the answer
arrives later, the engine queues a durable command in the bot chat
(`{kind: "bot-answer", exchangeId}`). It renders as an attributed card and starts a turn, so
the bot can follow up without the owner asking again. This reuses the command ledger, so it
also works while no UI is open.

## 8. Bot network

### 8.1 Routes

All routes sit behind bearer verification. They are one new `ROOM_ROUTES` entry with prefix
`bots`, binding `BOT_ROOMS`, and room `bot1/{botRoomId}`:

| Route | Caller | Does |
|---|---|---|
| `POST /bots/:id/publish` | owner | Claims the room (first caller is the owner) and sets the public profile. |
| `GET /bots/:id/inbox` | owner's host | Pulls undelivered inbox and outbox entries (after a nudge, and on reconnect). |
| `POST /bots/:id/invites` | owner | Creates an invite; returns `code = <botRoomId>.<secret>` once. |
| `DELETE /bots/:id/invites/:inviteId` | owner | Revokes an unused invite. |
| `POST /bots/:id/connect` | owner of `:id` | Redeems a peer's invite code on behalf of bot `:id`. |
| `DELETE /bots/:id/connections/:cid` | owner | Revokes a connection on both sides. |
| `POST /bots/:id/ask` | owner of `:id` | Sends a question on one of `:id`'s connections. |
| `POST /bots/:id/answer` | owner of `:id` | Answers (or declines) an exchange in `:id`'s inbox. |
| `POST /bots/:id/ack` | owner of `:id` | Acknowledges delivered inbox and outbox entries. |

### 8.2 Trust chain

A caller can only act through a bot it owns. Every route checks the caller against the
room's `owner`, the same claim-then-owner-only rule ChatRoom and DeviceRoom use. Cross-user
traffic then moves **DO to DO** over internal paths that the Worker never routes:

- `ask`: my BotRoom checks the connection is active and within rate limits, then calls the
  peer room's internal `deliver`. The peer room checks the same connection id from its side
  before queueing.
- `connect`: my BotRoom calls the peer room's internal `redeem` with the secret, my room id
  and profile. The peer room checks the hash, expiry and uses, creates its side, and
  returns its profile; my room creates mine.
- Revocation deletes both sides; a missing peer side counts as revoked.

Neither user's bearer ever reaches the other's room, and a peer cannot address a room it is
not connected to.

### 8.3 Delivery

Bots open no socket of their own. The host is already connected to its own DeviceRoom, so a
BotRoom that queues an entry sends that DeviceRoom a durable nudge (the existing queued
nudge, replayed when the host next joins), and the host pulls with `GET /bots/:id/inbox`.
Entries stay until acked after the engine has durably appended them to the peer thread.
Exchange state flows back to the asker as `queued → delivered → working →
(awaiting_review) → answered | declined | expired`, so `list_connections` and the UI can
show "A's bot is offline; queued". The BotRoom stores the host's device id at publish and
when the bot moves.

### 8.4 Cost

Durable Object requests bill 1:1 over HTTP and 20:1 for inbound WebSocket messages, and
duration bills while an object is awake (docs/operations/cloudflare-billing.md). The network
is designed to stay out of both:

- **No idle cost.** No per-bot socket or heartbeat (§8.3): one exchange is about 6–10
  billed requests and 15–25 rows written, and nothing bills between exchanges. BotRooms
  hibernate like every other room.
- **The real cost is the runs.** The bot chat and the answering side's peer thread sync
  through chat2 like any session, so one question costs about two agent turns of chat2
  traffic. Bot usage costs what the same amount of ordinary session usage costs.
- **Search stays local.** Scanning or indexing reads stored snapshots; it never holds
  chat2 rooms open.
- **Bounded worst case.** Per-connection rate limits (§8.5) cap a misbehaving peer, and
  `ask_group` costs one exchange per member.

Measure P2 with `scripts/ops/edge-billing-local.mjs` against two dev engines before it
ships.

### 8.5 Wire shapes

JSON text frames; the codec is mirrored in Rust and listed in `protocol/README.md` with
shared vectors.

```
ask      {t:"ask", exchangeId, connectionId, text, sentAt}
answer   {t:"answer", exchangeId, connectionId, status:"answered"|"declined"|"expired", text?, answeredAt}
state    {t:"state", exchangeId, state}
link     {t:"link", connectionId, op:"added"|"revoked", peer?:{botRoomId, name, ownerName}}
```

Limits: question ≤ 8 KiB, answer ≤ 64 KiB. Defaults per connection: 30 questions per hour,
200 per day, and at most 3 in flight.

### 8.6 Privacy on the Edge

Message bodies cross the Edge in plaintext and rest in DO SQLite until they are acked or
expire. That matches how chat2 handles transcripts today, and it is stated in the UI. End-
to-end encryption with per-bot X25519 keys exchanged at redeem time is a later phase (P5).
It needs no new routes, only opaque `text` fields.

## 9. Groups

- **P4a, owner labels.** A group is the owner's label over their own connections (a
  `botGroups` registry row: name and connection ids). `ask_group` fans out. Nothing is
  shared, so it needs no Edge change.
- **P4b, shared groups.** A `GroupRoom` DO holds membership. Joining a group by invite
  connects the joining bot to every member, and each member's side gets the group's default
  policy, which members can still narrow. This is the "every project bot in the team is
  connected" setup with one invite instead of N² invites.

## 10. UI

Desktop:

- **Bot strip** (P0): a row of 30px avatar squares between the sidebar header and the
  project cards, one per bot, their edge lined up with the project glyphs; empty, it is
  not shown. The open bot is ringed in its own colour, a corner dot carries the chat's
  status, a hover shows the name, and a right-click opens Open / Settings… / Delete….
  Project windows have no strip. Bots are created from the sidebar header's `+`, which
  opens a menu: New project… / New bot….
- **Bot palette** (P0, create and edit), drawn like the Quick Chat and Add project
  palettes: a frosted header band, the form on the left and a "Run on device" rail on
  the right (two-line rows: the device name, then This device / Online / Offline / the
  version it needs; fixed after creation), and a Cancel / Save footer with Delete on the
  left when editing. The form holds the name beside a live avatar, the avatar picker
  (twelve robot faces, a few symbols or the initial, neutral until picked, and a colour),
  Permissions (project access Selected / All / None, Read conversations, Archived
  sessions) with a live access count, and instructions.
- **Bot conversation** (P0): an ordinary session tab, with these differences. The tab
  wears the bot's avatar. The status strip over the composer carries a quiet access note
  ("All projects · reads conversations") that opens the bot's settings. A new bot opens
  on a start screen: its avatar, name and host, and a 2×2 grid of suggested questions a
  click asks. Bot tool calls read as what they did ("List sessions · all", "Search
  sessions · "invoice"", "Read session · <title>"; "Looked over your work · read 2
  sessions"), and the bot cites sessions as `cypher://session/<id>` links that open in
  the app. The rail has no terminal and no dock; the composer asks "Ask <bot>…" and
  leaves out Attach files, `/permissions` and the commands the engine refuses in a bot
  chat (`/scripts`, `/orchestrate`, `/goal`).
- **Bot dock tab** (P2+, right dock of a bot chat) with four panes:
  - *Scope*: project tree grouped by device with checkboxes, per-session overrides,
    capability toggles, and a live "this bot can see N sessions in M projects" summary.
  - *Connections*: peer bot, owner, reply mode (Auto/Review), connection scope, rate limit,
    and revoke. "Invite" creates a link or code; "Connect" redeems one.
  - *Activity*: every exchange, asked and answered, newest first. Each links to its peer
    thread and shows the tool calls the answer used.
  - *Review*: drafts waiting for approval, with Approve, Edit and send, and Decline.
- Bot management RPCs take `targetDeviceId` and are forwardable, so every pane works from
  any of the owner's devices; registry edits sync on their own.

iPhone (P5): the bot list, the bot chat, and Review approvals, driven by a push
notification (`bot-review`) through the existing notifications pipeline.

## 11. RPC

New methods, each one constant plus one `SPECS` entry in `cypher_rpc::methods`. P0 has
the first three and two `Mutate` ops:

| Method | Kind | Notes |
|---|---|---|
| `CreateBot` | unary, forwardable | Runs on the host: folder, bot chat and row in one batch; replies with the `Bot`. |
| `WatchBots` | stream | The registry's bot rows. |
| `BotTool` | unary | The extension's single entry point: `{chatId, tool, args}` → `{text}`. The engine resolves the bot from `chatId`, so a chat cannot claim another bot; a refusal is returned as text for the model. |
| `Mutate updateBot` / `deleteBot` | unary | LWW edits from any device; delete takes the bot chat with it. In P2 delete also revokes all connections and the room. |
| `BotAsk` | stream, local only | `ask_bot` / `ask_group`: exchange states until answered or `wait_s`. |
| `CreateBotInvite`, `RevokeBotInvite`, `RedeemBotInvite` | unary, forwardable | |
| `UpdateBotConnection`, `RevokeBotConnection` | unary, forwardable | Reply mode, connection scope, rate limits. |
| `WatchBotActivity` | stream, forwardable | Exchanges and review drafts for one bot. |
| `ResolveBotReview` | unary, forwardable | Approve, edit-and-send, or decline. |
| `PreviewBotScope` | unary, forwardable | Counts and lists of what a scope would expose, for the Scope pane. |

`BotTool` and `BotAsk` are not forwardable: they are only meaningful on the host.

## 12. Phases

| Phase | Delivers | Main code |
|---|---|---|
| **P0: local bot** (done) | `bots` rows, bot chat, bot spawn mode with tool allowlist, four read tools over the registry and transcripts (bounded scan, no index), scope enforcement, the bot strip and dialog, use from other owner devices. | `cypher-proto` `bot.rs`, `cypher-doc` registry, `engine/bots/`, harness spawn and `bot-tools.ts`, `cypher-ui` `shell/bots/` |
| **P1: search and progress** | FTS index behind `search_sessions`, `session_activity`, `project_git` over the relay. | `engine/bots/index.rs` |
| **P2: network** | `BotRoom` DO, publish, invites, connections, ask/answer in Auto mode, peer threads, exchange states, late answers, Activity and Connections UI. | `apps/edge/src/bots/`, `cypher-sync` bot client, protocol vectors |
| **P3: review and safety** | Review mode, outgoing scrub, rate limits, notifications for pending reviews and incoming connections. | engine, edge, UI |
| **P4: groups** | P4a owner labels and `ask_group`; P4b shared `GroupRoom`. | registry, edge |
| **P5: iPhone and E2E** | iOS bot list, chat and review; end-to-end encrypted exchanges. | `apps/ios`, edge codec |

Each phase ships behind no flag of its own: P0 and P1 are local and private by design, and
the network appears in P2 only for synced accounts.

Tests per phase follow the repository's tiers: scope-filter unit tests (the core of the
security model, including the "not found" rule and bot-chat exclusion), engine integration
tests with the mock harness driving bot tools, workerd tests for BotRoom (ownership,
redeem, revoke, cross-room delivery, rate limits, expiry), shared frame vectors, and
`scripts/e2e-smoke.sh` extended to two users with one bot each.

## 13. Open questions

- **Owner-only action tools.** Should the owner's bot be able to *act*, for example start a
  session in a project, queue a message, or archive? This would be powerful for the global
  view, but it needs a separate capability and must never be reachable from peer threads.
- **Identity shown to peers.** Owner display name from WorkOS, email, or only a name the
  owner types? The default proposal is the WorkOS display name, editable per bot.
- **Host offline for long.** Should a bot have a fallback host device, so questions are not
  queued while the only host sleeps? This needs the claim and lease logic deliberately left
  out of P0–P2.
- **Answer provenance.** Should peers see which sessions an answer cited (titles only), or
  only the text?
- **Retention.** Is 7 days right for undelivered questions, and should peer threads expire
  locally?
