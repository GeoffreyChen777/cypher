# Agent bots

Status: **P0 and P1 implemented.** P0: local, read-only bots (definition, scope, the bot
chat and its read tools, the palette). P1: engine-stamped origins, the role-aware run
profile, per-origin prompts with the untrusted-data rule, slash commands by origin, the
unattended run mode, the search index, `session_activity` and `project_git`. The first
Botboard slice (§13.1: mode switch, picker, Status and Projects cards, detail panel) is
built. The bot network's Edge and engine side (§11, P4) is built. The rest is design;
§16 is the build order. Where the built code differs from the
full design, the section says so.

A bot is a **coordinator**: a long-lived agent responsible for everything in the projects
it is given. Inward, it takes instructions from its owner and gets work done across those
projects: it plans a task, works in a project itself or starts sessions to do the work,
watches them, and reports back. Outward, it answers questions from the bots of the people
its owner is connected to, from what the owner allows them to see.

"Fix bug X in project A, then check that bringing A's change into project B breaks
nothing" is one instruction to the bot. The bot plans it, hands it to an orchestration
thread, and stays free for the next question while the thread creates a worktree in A,
starts a session there to fix the bug, starts a session in B to run its tests against
A's fix, and reports the outcome.

## 1. Goals and non-goals

Goals:

- **One agent over many projects.** A conversation that sees every granted project and
  session, on every device in the profile, and can act across them.
- **Work that runs on its own.** Large tasks run in their own threads, in parallel, while
  the bot stays available. They survive restarts and run with no UI open.
- **Permissions by origin.** What a thread may do depends on who started it: the owner,
  an unattended routine, or another user's bot. The engine enforces it on every call; the
  model cannot widen it (§4).
- **Bot-to-bot questions across users and organizations.** Connections are made by invite,
  either side can revoke them, and each connection has its own policy.
- **Execution stays on the owner's machines.** Bots run on an engine the owner picked. The
  Edge relays messages between bots and never runs a model or reads a session.
- **Usable from every device.** A bot hosted on Mac A is used from Mac B (and later the
  iPhone) the same way a remote session is.

Non-goals:

- **Work on a peer's behalf.** Another user's bot can ask questions; it can never start,
  steer or change anything, directly or through the owner's bot.
- **Reading another user's sessions.** Peers receive text answers, never transcripts.
- **A public bot directory.** Discovery is by invite only.
- **Hosting on iOS.** The iPhone is a remote control, as it is for sessions.

## 2. Roles

| Role | Is | Does |
|---|---|---|
| **Owner** | The user | Instructs the bot, approves plans, reviews results. |
| **Coordinator** | A bot | Plans, coordinates and reports across its projects. Works directly in local projects when the owner allows it; delegates everything else to sessions. |
| **Worker** | A session | One task in one checkout, with real tools under its sandbox. Usually short-lived. |
| **Peer** | Another user's bot | Asks questions. Read-only, through the connection's narrower scope. |

Sessions stay what they are today, and the interface keeps "session" and "bot" separate:
a session is an agent working on one task, a bot is your assistant across your work.
Root sessions do not talk to one another (subagents still message inside their own task
tree); coordination between sessions goes through the bot.

## 3. Concepts

| Term | Meaning |
|---|---|
| **Bot** | A named agent owned by one user: definition, scope, capabilities, host device and its chats. A user may own many, one per area or audience. |
| **Host device** | The Mac or Linux engine that runs the bot, picked at creation. iOS devices cannot host. |
| **Thread** | One of the bot's chats. Every thread has an **origin** that fixes its permissions (§4). |
| **Bot chat** | The owner's main conversation with the bot (origin *owner*). |
| **Orchestration thread** | A hidden child chat of the bot chat that carries out one large task (origin *owner*), §6. |
| **Routine thread** | A hidden chat in which one routine runs (origin *routine*), §8. |
| **Peer thread** | A hidden chat per connection where a peer's questions run (origin *peer*), §11. |
| **Scope** | Which projects and sessions the bot may read (§5). |
| **Capabilities** | Independent switches for what the bot may do beyond reading (§4.2). |
| **Notebook** | The bot's durable memory: notes it reads at the start of each owner turn (§7). |
| **Routine** | A scheduled or event-triggered bot job whose output is a message (§8). |
| **Recurring task** | A scheduled job on a project whose every run starts a fresh session (§8). |
| **Connection** | A mutual link between two bots of different users, created by invite (§11). |
| **Exchange** | One question from a peer bot and its answer. Audited on the answering side. |

## 4. Origins and permissions

### 4.1 Origin decides what a thread may do

| Thread origin | Tools | Approvals |
|---|---|---|
| **Owner** (bot chat, orchestration threads) | Read tools; coordination tools if *Coordinate* is on; Pi's own tools (shell, files) in local projects if *Work directly* is on | The owner's permission control and sandbox, exactly as in a session, plus the taint rule (§4.3) |
| **Routine** | What the owner granted when creating the routine, never more than an owner thread | Never waits: anything that would ask for approval fails the run and notifies the owner |
| **Peer** | Read tools only, scope ∩ connection scope | None needed: nothing it can call acts |

The engine stamps a thread's origin when it creates the thread and stores it on the chat
row's `bot` marker. Origin is never inferred from content and never changes: a peer's
question always lands in its peer thread, nothing a peer sends can create or continue an
owner thread, and an orchestration thread can only be started from an owner thread.

### 4.2 Capabilities

Capabilities are independent switches on the bot, not a ladder; turning one on implies
nothing about the others.

| Capability | Lets owner threads | Default |
|---|---|---|
| **Observe** | Read projects and sessions in scope | Always on (scope decides what) |
| **Work directly** | Use Pi's shell and file tools in the bot's local projects | On for new bots; off gives a read-only reporting bot |
| **Coordinate: start** | Create worktrees and start sessions in its projects, on any owner device | Off; one switch |
| **Coordinate: steer** | Steer, answer or stop sessions it did not start | Off; ships after *start* |
| **Report** | Run routines (§8) | Per routine |
| **Represent** | Answer connected peers (§11) | Per connection |

Memory (§7) is not a capability: every bot has a notebook.

### 4.3 The taint rule

Origin controls who may instruct a thread, not what text reaches its model. Owner threads
read worker transcripts, web output and, through `ask_bot`, other users' answers; any of
these can carry injected instructions. So:

- Tool results that carry such text are marked in the prompt as quoted data, never
  instructions (§10.5).
- Permission control stays on in owner threads. "Full tools" means the rules of a
  session, not no rules.
- A thread that has received a peer answer is **tainted** for the rest of its run: actions
  that would otherwise run silently (commands outside the sandbox, writes outside its
  projects, starting or steering sessions) require the owner's approval.

### 4.4 Enforcement

- **Spawn time** fixes what a run *can* call: the tool allowlist, the extensions, the
  system prompt and the workspace roots, all derived from (bot, thread origin). A run
  cannot gain a tool mid-run.
- **Call time** decides whether a call is *allowed*: every bot tool re-reads the bot row
  and checks scope and capabilities, so turning a switch off applies to the next call of
  a running thread. Each coordination call is written to the bot's audit log.
- **Workspace.** An owner thread with *Work directly* gets the checkouts of its scoped
  projects **on its host** as permission control's workspace roots; its cwd stays the
  bot's private folder. Projects on other devices are reachable only by starting a
  session there (*Coordinate*). Peer and routine threads without *Work directly* keep the
  empty folder and no Pi tools.
- **Slash commands.** The engine refuses `/scripts`, `/orchestrate` and `/goal` in owner
  threads, because codemode honours `/scripts` under `--tools`; *Work directly* may lift
  this. Peer and routine threads accept no slash commands at all (built).

As built (P1): the origin rides the chat row's `bot` marker (`{botId, origin}`; a missing
origin reads as owner, an unknown one as peer). Only the engine writes it: `CreateBot`
stamps owner, and no `Mutate` op, `CreateChat`, fork or side-chat path takes or copies a
marker. `bots/profile.rs` derives the run profile from (chat, bot row, origin). Every
origin gets the read tools for now, and `workspace_roots` stays empty until *Work directly*
ships.

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

`cypher_proto::BotScope` holds the rule; the engine enforces it and the palette previews
it ("Access to 12 sessions in 3 projects").

- `projects.mode` is `"none"` (the default), `"selected"`, or `"all"`. `"all"` covers
  projects created later. `"selected"` names spaces, which are (device, folder) pairs, so a
  project on two devices is granted per device.
- A granted project grants its sessions, including future ones. `excludeSessions` removes
  single sessions; `includeSessions` grants single sessions in ungranted projects. The P0
  palette edits projects and the two switches and keeps the session lists as they are.
- Subagent child chats follow their root parent; a broken or cyclic chain hides the chat.
- `transcripts: false` limits reads to titles, latest-message previews and status.
- Every chat with a `bot` marker is excluded from every bot's scope, including its own:
  otherwise one peer's question could surface another peer's exchange.
- Sessions a bot started belong to their project like any session, so they are in scope
  exactly when their project is.

### 5.2 Effective scope

| Thread | Reads | Acts in |
|---|---|---|
| Owner | bot scope | bot scope (local checkouts directly, any device through sessions) |
| Routine | bot scope ∩ routine scope | as granted to the routine |
| Peer | bot scope ∩ connection scope | nothing |

A routine's or connection's scope can only narrow the bot's. Anything outside the
effective scope answers exactly like something missing, so the model cannot probe for it.

## 6. Orchestration

### 6.1 Flow

```
Owner: "Fix bug X in A, then check B against A's fix"
  └─ bot chat ─ plans ─► plan card (approve / edit / cancel)
                           └─ approve ─► orchestration thread (hidden child chat)
                                          ├─ create worktree in A
                                          ├─ start session in A ("fix bug X")  ─► worker 1
                                          ├─ wait, read its result
                                          ├─ start session in B ("run tests against A's branch") ─► worker 2
                                          ├─ wait, read its result
                                          └─ report ─► result card in the bot chat
```

1. The owner asks in the bot chat. For a task that needs more than a couple of steps the
   bot writes a plan: steps, projects, devices, models, and how the steps depend on each
   other (`propose_plan`).
2. The plan shows as a card. In the first version every plan waits for the owner's
   approval; auto-start becomes a per-bot setting later.
3. On approval the engine creates an **orchestration thread**, a hidden child chat of the
   bot chat with origin *owner*, and queues its first run. The bot chat is free again.
4. The thread does the work: small steps directly in local projects (*Work directly*),
   real changes and anything on another device in worker sessions (*Coordinate*).
5. When it finishes, fails or is stopped, the result lands in the bot chat as a card, and
   the bot can follow up.

### 6.2 The thread starts from a brief

The orchestration thread does not inherit the bot chat's history. It starts from a brief
the engine assembles: the approved plan, the owner's instruction verbatim, the relevant
notebook entries, and its permissions in plain words. Its context stays small, and nothing
from unrelated conversations leaks into the work.

### 6.3 Direct work or a session

The bot's instructions give it this trade-off:

- **Directly:** quick, small things in a local project: run a test, read a file, check
  `git log`, a one-line fix.
- **A session:** real changes that deserve their own transcript, diff and worktree; work on
  another device; steps that run in parallel.

Worker sessions are ordinary sessions in their project, marked with who started them
(`startedBy: {botId, threadId}` on the chat row) and shown as "Started by Bob · <task>".
The owner can open, steer or stop them like any session.

### 6.4 Dependencies between projects

"Test B against A's fix" needs B to see A's change. The plan must name one of:

- a **path dependency** onto A's worktree, when A and B are on the same device;
- a **git ref**: A's branch pushed to a remote B can fetch, across devices;
- a **published version**, when the projects already depend on each other that way.

A local path never crosses devices; the plan step says which mechanism it uses, and the
worker's prompt carries it.

### 6.5 Blocked workers

Workers started by an orchestration run unattended by default: anything that would ask for
approval fails the step instead of waiting. A worker that asks the user a question is
surfaced to the orchestration thread, which answers from the plan or passes it to the
owner. Nothing stalls silently.

### 6.6 Control and limits

- A live **progress card** in the bot chat: the plan, each step's state, links to its
  sessions, and elapsed time.
- **Stop** ends the thread and interrupts every session it started.
- Limits per bot: concurrent orchestrations (3), sessions per orchestration (8), turns per
  worker, and a total time budget, all configurable.
- Orchestration threads survive engine restarts like any chat: their transcript and the
  durable command queue carry the state.

## 7. Memory

- **The notebook** is a small file in the bot's private folder, read at the start of each
  owner turn and edited with two tools (`read_notes`, `write_note`). Entries are short
  facts and decisions: "the release is blocked on the migration", "Alice owns auth".
- **Owner threads only.** Peer threads can neither read nor write the notebook, so it can
  never bypass a connection's scope or carry a peer's injected text into an owner thread.
  Routine threads read it, and write to it only when the routine says so.
- **Provenance.** Every entry records where it came from: the owner, or the session or
  thread whose content produced it. The notebook view shows the source and lets the owner
  delete any entry.
- **Phasing.** First the owner writes notes ("remember that…"); bot-written notes follow
  once provenance and the notebook view exist.

## 8. Routines and recurring tasks

Two kinds of scheduled job, run by one engine scheduler on the host:

| | Routine (bot) | Recurring task (project) |
|---|---|---|
| Example | "Every morning: what moved, what's stuck, what needs me" | "Every night: run the tests and fix anything flaky" |
| Runs in | The routine's own hidden thread | A fresh session per run, ideally in a new worktree ("Nightly tests · Oct 15") |
| Output | A message card in the bot chat, optionally a notification | Changes in a checkout, reviewed like any session |
| Permissions | Granted per routine, never more than an owner thread; no `ask_bot` | The task's own model, sandbox and checkout settings |

- **Triggers:** a schedule, and later events ("a session errors", "a session waits on you
  for 10 minutes", "CI failed on main").
- **Unattended:** a routine or recurring run never waits for approval. Permission prompts
  are denied automatically and the run settles as failed, with a notification.
- **Budgets:** turns per run and runs per day per routine, and the bot's `paused` flag as a
  kill switch for all of its routines.
- **Missed runs:** a device that slept through a schedule runs the job once on wake and
  skips the rest.
- **Delivery:** the scheduler queues a durable command, so jobs run with no UI open.
- Hidden routine threads sync through chat2 like any chat; each run has the cost of one
  agent turn or more.

## 9. Bot runtime on the host device

The engine module `crates/engine/src/bots/` serves the bots whose `hostDeviceId` is this
device. As built:

- **Creation.** `CreateBot` runs on the chosen host (it is relay-forwardable): it makes the
  bot's private folder (`<profile store>/bots/<botId>`, 0700), then writes the bot chat
  and the bot row in one registry batch. Edits and deletes are `Mutate` ops (`updateBot`,
  `deleteBot`) any owner device can make; a rename retitles the bot chat, and a delete
  tombstones the bot with its chat, after which the host stops the run and removes the
  folder.
- **Runs** go through the normal sessions engine: steering, interrupts, queued sends, the
  run journal and crash resume apply unchanged, and the owner's other devices drive a bot
  chat through chat2 and the command plane like any remote session. A run recreates the
  bot's folder if it is gone.
- **Reading sessions.** The search index and the search fallback read transcripts through
  `DocHost::read_transcript`: the open handle when there is one, else the stored snapshot.
  It joins no room, starts no task and leaves the warm-doc cache alone. `read_session`
  wants the freshest copy, so for a chat another device hosts it opens the doc (joining
  its chat2 room) and gives it a few seconds to catch up.
- **Search index.** `<profile store>/bots-index.sqlite` (beside `<store>/bots`) is a
  SQLite FTS5 index of message text (trigram tokenizer): `entries` rows keyed by (chat id, message index) with
  the role, the text in an FTS5 table sharing their rowid, and one watermark row per chat
  (row activity, snapshot save time, whether a copy existed). It holds only chats some bot
  hosted here may read message text from (the union of their scopes; never bot chats).
  `bots/index.rs` keeps it current in the background: at startup, after chat or bot row
  changes (1.5 s debounce) and on a 2-minute tick (another device's chat can gain a fresher
  snapshot with no row change), it re-reads at most 40 chats per pass whose watermark
  moved, newest first, and drops chats that left every scope or were deleted. All
  SQLite and transcript work runs off the runtime; a corrupt or outdated file is deleted
  and rebuilt. Scope is filtered at query time against the caller's current view, so a
  narrowed scope applies at once.
- **Search.** `search_sessions` asks the index for messages containing every word as a
  substring, any case, as the P0 scan did, so a word matches inside identifiers like
  `BotScope`; words under three characters, which trigrams can't match, filter with
  `LIKE`. It ranks sessions by their best message's relevance (title matches first), then
  recency, and still matches titles and previews from the rows. Sessions the index does
  not hold yet are scanned directly, the newest 400 of them; the answer says how many
  were not searched, and how many have no copy on this device (matched by title only).
  With no index at all the scan still works.
- **Activity.** `session_activity` reads only registry rows: chat rows give creation,
  last activity and the latest preview, session rows give the current status, and a
  settled row's `updatedAt` stands in for when its last run ended (shown only when that
  run left a message inside the window). No run history is persisted for it.
- **Git facts.** `project_git` reads a space on this device with git directly (branch,
  `@{upstream}` ahead/behind, `git log`, the checkout diff snapshot). A space on another
  device goes through that device's forwardable `ListRefs`, `ListGitHistory` and
  `GetCheckoutDiff` over the device relay, within 10 s; offline, the tool says so. Those
  RPCs carry no ahead/behind, so it is estimated from the 200-commit history page against
  the `<remote>/<branch>` decoration and marked as an estimate.
- **Run profiles (built in P1).** `RunHostContext` carries one `RunProfile`: `Root`,
  `Child(ChildRunEnv)` or `Bot(BotRunEnv)`, so a run can no longer be both a child and a
  bot. `BotRunEnv` holds the bot id, origin, tool allowlist, system prompt and workspace
  roots, derived from (chat, bot, origin) by `bots::profile::bot_env`. The pi harness turns
  a profile into `--tools`, `-e`, `--append-system-prompt` and env in one place, and every
  spawn first clears the env of the profiles it is not. Scope and capabilities stay out of
  the profile, because they are checked per call (§4.4). The persisted
  `ChildAgentProfile` keeps its synced shape.
- **Unattended runs (built in P1).** `bots::profile::is_unattended` (routine threads for
  now) switches a run's input bridge: every question and approval is answered at once
  with an empty answer, which ask_user reads as cancelled and the permission extension
  as "no approval was given", and none surfaces as a pending card. The first one a working
  turn asks stops the run: the transcript gets "This run is unattended, so it couldn't ask
  for approval to …. It stopped.", the status ends Errored, and the engine hands an
  `UnattendedStop` (chat id, reason) to its unattended-stop listener, where the owner's
  notification goes once routines exist (§8).

## 10. Bot tools

The extension `crates/harness/src/pi/bot-tools.ts` is embedded in the engine binary like
`engine-client.mjs` and loaded with `-e` into bot runs only, so bot tools never appear in
other sessions and need no Runtime release. Each tool is one `BotTool` call (`{chatId,
tool, args}` → `{text}`) over the private socket; the engine resolves the bot and thread
from the chat id, enforces scope, origin and capabilities, and formats the answer. Answers
are plain text, bounded to 48 KiB.

### 10.1 Read tools (every origin; all six ship)

| Tool | Returns |
|---|---|
| `list_projects` | Granted projects: name, id, device, folder, git, session count, working count, last activity. |
| `list_sessions` | Root sessions in scope, newest first, filterable by project, `active` and `since_hours`, with status and latest-message preview. |
| `search_sessions` | Sessions whose title, preview or messages contain every word (substrings), best match first; up to three excerpts each, with message indexes. |
| `read_session` | Up to 60 messages of one session, latest by default or from an index; tool calls as one-line summaries, thinking left out. |
| `session_activity` | What changed in the last `since_hours` (default 24, at most 336), optionally in one project: sessions started, sessions with new messages with their current status and latest preview, runs that ended; grouped by project, newest first, at most 60 sessions. |
| `project_git` | One granted space: branch, ahead/behind its upstream, the last 10 commits (short sha, subject, author, time) and working-tree changes by file. Another device's space needs that device online. |

### 10.2 Direct work (owner threads with *Work directly*)

Pi's own tools (`bash`, `read`, `write`, `edit`, `grep`, `find`, `ls`) under permission
control, with the bot's local scoped checkouts as workspace roots (§4.4).

### 10.3 Coordination (owner threads with *Coordinate*)

| Tool | Does |
|---|---|
| `propose_plan` | Puts a plan card in the bot chat; approval starts an orchestration thread. Bot chat only. |
| `create_worktree` | A new worktree in a granted project, on that project's device. |
| `start_session` | A session in a granted project or worktree, with a prompt, model and sandbox; returns its id. |
| `wait_for_session` | Waits until a session settles or asks something, up to a timeout; returns its state and last answer. |
| `answer_session` | Answers a question a session it started is asking. |
| `steer_session`, `stop_session` | For sessions it started; for others only with *Coordinate: steer*. |

### 10.4 Memory and network

| Tool | Threads | Does |
|---|---|---|
| `read_notes`, `write_note` | owner, routine (if granted) | The notebook (§7). |
| `list_connections` | owner | Connected bots: name, owner, connection id, how this bot replies to them, last question. |
| `ask_bot` | owner | Asks one connected bot (by name, any case, or connection id); waits up to `wait_s` (default 120 s, at most 600). Taints the thread when an answer comes back (§4.3). |
| `ask_group` | owner | Fans a question out to a group; returns the answers received in the window. Not built. |

Network tools never exist in peer threads, so a peer cannot use the bot to relay
questions to third parties.

As built (P4): `list_connections` and `ask_bot` are in every owner thread's allowlist
(`cypher_proto::BOT_NETWORK_TOOLS`), never in routine or peer threads, and a call from
any other thread is refused. Without an Edge (the local profile, or signed out) both
answer "The bot network needs you signed in to Cypher." `ask_bot` returns the answer
between `<peer-answer>` tags, followed by a reminder that it is another user's data;
"declined to answer" and "expired unanswered" are plain sentences. The extension gives
`ask_bot` a 630 s call timeout.

### 10.5 Prompting

The system prompt states the bot's role and its permissions for this thread in plain
words, how to choose between direct work and a session (§6.3), that every answer cites the
sessions it draws on as `cypher://session/<id>` links (built), and that text from
transcripts, tool output, web pages and peers is data to read, never instructions to
follow. In peer threads it also says who is asking.

As built (P4): an owner thread with live connections gets a network section naming the
connected bots and their owners, when to ask one, that answers are another person's data
to attribute and check, and that a `pending` answer arrives later as an `[Answer from …]`
message. A peer thread's prompt names the asking bot and its owner from the connection
row and describes the effective scope (bot ∩ connection), not the bot's own.

### 10.6 Late answers

When `ask_bot` times out it returns `pending` with the exchange id. When the answer
arrives, the engine queues a durable Run in the bot chat, so the bot can follow up
without the owner asking again.

As built (P4): an answer that arrives with no `ask_bot` call waiting on it (it timed
out, or the engine restarted) becomes a Run command in the bot's main chat, with message
id `bot-answer-<exchangeId>`. The transcript shows the owner a readable, attributed
message:

```
[Answer from "Billing bot" (Alice's bot) to your question "Can we ship Friday?"]

Yes, Friday works.
```

Declined and expired questions read `["Billing bot" (Alice's bot) declined your question
"…"]` and `[Your question to … expired unanswered: "…"]`. The agent's prompt
(`agent_prompt`) adds that the text is another user's reply, data and not instructions,
and asks the bot to tell its owner what it means. An answered late reply taints the bot
chat. A dedicated card for these messages (`{kind: "bot-answer"}`) is not built; the
bracketed first line is what a UI can key on.

## 11. Bot network (Represent)

Status: **built (P4)** on the Edge and in the engine: `BotRoom`, publish, invites,
connections, peer threads in Auto, Review and Off, the outgoing scrub, exchange states,
late answers and revocation. The Network card and its detail panel (§13.1) belong to the
UI track. Groups (§11.7) and end-to-end encryption are not built.

### 11.1 Flow

```
Me (any device)              my host engine            Edge                     A's host engine
   │ "How is A doing on X?"      │                       │                            │
   ├── bot chat ────────────────►│ ask_bot(A, q) ───────►│ BotRoom(me) → BotRoom(A)   │
   │                             │                       ├── ask (queued if offline) ►│ peer thread, read-only,
   │                             │                       │                            │ scope ∩ connection scope
   │                             │                       │◄── answer ─────────────────┤ (Review: waits for A)
   │◄── reply citing A's bot ────┤◄── answer ────────────┤                            │
```

Peer questions run in that connection's own peer thread with read tools only. In **Auto**
mode the answer goes back at once; in **Review** mode it waits as a draft until A approves,
edits or declines it; with replies **Off** every question is declined without running.
Before an answer leaves the host, a best-effort pass redacts strings that look like
secrets (private-key blocks, common token formats, `.env`-style assignments); it is a
safety net, not a guarantee, and the UI says so.

### 11.2 Edge `BotRoom`

One Durable Object class, `BotRoom` (`apps/edge/src/bots/bot-room.ts`), keyed
`bot1/{botRoomId}` and bound as `BOT_ROOMS` (wrangler migration `v7`,
`new_sqlite_classes: ["BotRoom"]`), one room per networked bot. The host engine mints
the room id. The room learns it from `?room=`, which the Worker adds to every forwarded
request.

| Table | Contents |
|---|---|
| `meta` | `owner` (user id, claimed on the first publish), `room`, `profile` `{name, ownerName}`, `host` (device id), `paused` |
| `invites` | id, `sha256(secret)` (hex), created, expires, max uses, uses, revoked |
| `connections` | id, peer room id, peer name and owner name, created, revoked, the invite it came from, and `link_pending` (`added` or `revoked`, until the host acks it) |
| `inbox` | questions addressed to this bot: exchange id, connection, `text` (dropped once the host acks it), state, sent, acked |
| `outbox` | questions this bot asked: exchange id, connection, state, the answer `text` (dropped once acked), answered, the state the host last acked |

The per-connection limits count `inbox` and `outbox` rows, so there are no separate
counters: settled rows stay a day without bodies, then go. An exchange not settled 7
days after it was sent expires in both rooms (both use the asker's `sentAt`); an alarm
fires at the next such deadline, and every request sweeps first. A settled answer the
host never pulls is dropped after 7 days. Expired invites are dropped after 30 days.

### 11.3 Routes

All routes sit behind bearer verification as one `ROOM_ROUTES` entry with prefix `bots`
(closed: anything else under `/bots/:id/` answers 404):

| Route | Caller | Does |
|---|---|---|
| `POST /bots/:id/publish` | owner | Claims the room on first use; sets `{name, ownerName, hostDeviceId, paused}`. |
| `GET /bots/:id/inbox` | owner's host | `{entries: [frame…]}`: pending link changes, unacked questions, and the states and answers of its own questions (at most 100 of each). |
| `POST /bots/:id/invites` | owner | `{ttlMs?, maxUses?}` (defaults 7 days and 1; clamped to 10 min–30 days and 1–100) → `{inviteId, code, createdAt, expiresAt, maxUses}`, `code = <botRoomId>.<secret>` once. |
| `DELETE /bots/:id/invites/:inviteId` | owner | Revokes an invite. |
| `POST /bots/:id/connect` | owner of `:id` | `{code}` → `{connectionId, peer, createdAt}`: redeems a peer's invite on behalf of bot `:id`. |
| `DELETE /bots/:id/connections/:cid` | owner | Revokes a connection on both sides. |
| `POST /bots/:id/ask` | owner of `:id` | `{exchangeId, connectionId, text}` → `{exchangeId, sentAt}`. The engine mints the exchange id, so a retry is the same exchange. |
| `POST /bots/:id/answer` | owner of `:id` | `{exchangeId, status: "answered"\|"declined", text?}` → `{delivered}`. |
| `POST /bots/:id/state` | owner of `:id` | `{exchangeId, state: "working"\|"awaiting_review"}`: progress, forwarded to the asker. |
| `POST /bots/:id/ack` | owner of `:id` | `{asks?: [exchangeId], states?: [{exchangeId, state}], links?: [connectionId]}`. Acking an ask marks it `delivered` for the asker. |

Errors are `{error, message}`: `not_published` and `no_connection`/`no_exchange` (404),
`forbidden` and `same_owner` (403), `invite_invalid` (404, one answer for wrong, expired,
revoked and used-up codes), `already_connected` and `settled` (409), `too_large` (413),
`too_many_in_flight` and `rate_limited` (429), `paused` (503).

The rooms call each other at `/peer/redeem`, `/peer/ask`, `/peer/answer`, `/peer/state`
and `/peer/revoke`. No `ROOM_ROUTES` entry reaches these paths, and a room refuses them
on any request that carries the Worker's user header.

### 11.4 Trust chain

A caller can only act through a bot it owns: every route checks the room's `owner`, the
claim-then-owner-only rule ChatRoom and DeviceRoom use. Cross-user traffic moves **DO to
DO** over internal paths the Worker never routes: `ask` checks the connection and rate
limits on both sides before queueing; `connect` redeems the hashed secret in the peer room;
revocation deletes both sides. Neither user's bearer reaches the other's room, and a peer
cannot address a room it is not connected to.

As built: every peer call names the calling room, and the callee accepts it only for a
live connection it recorded with that room. `connect` refuses a code from a room of the
same owner (bots of different people only) and a second live connection between the same
two rooms. A room nudges devices as its owner, and a DeviceRoom accepts only its own
owner, so a room published onto someone else's device id nudges nothing. A peer that
can't be reached on revoke keeps a dangling connection, but the revoking room refuses
every question on it.

### 11.5 Delivery and wire shapes

Bots open no socket of their own. A BotRoom that queues an entry sends the host's
DeviceRoom a durable nudge, and the host pulls with `GET /bots/:id/inbox`; entries stay
until acked after the engine has written them down. Exchange state flows back as
`queued → delivered → working → (awaiting_review) → answered | declined | expired`.

```
ask      {t:"ask", exchangeId, connectionId, text, sentAt}
answer   {t:"answer", exchangeId, connectionId, status:"answered"|"declined"|"expired", text?, answeredAt}
state    {t:"state", exchangeId, state}
link     {t:"link", connectionId, op:"added"|"revoked", peer?:{botRoomId, name, ownerName}, inviteId?}
```

`inviteId` names the invite a peer redeemed, on the inviting side. The codec is
`cypher_proto::bot_wire` and `apps/edge/src/bots/bot-frames.ts`, both pinned by
`protocol/vectors/bot-frames-v1.json`. Limits: question ≤ 8 KiB, answer ≤ 64 KiB (UTF-8
bytes); per connection 30 questions an hour, 200 a day, 3 in flight, checked by both
rooms.

As built: the nudge is `POST /nudge {botRoomId}` on the host's DeviceRoom (sent DO to DO
as the room's owner). It reaches the host as a device frame of kind `bots` with payload
`{botRoomId}` (`cypher_rpc::device_room::BOTS_NUDGE_KIND`; older engines skip the unknown
kind) or, while the host is offline, waits in `pending_bot_nudges` (at most 64) and is
replayed when the host joins. The host relay hands it to `Bots::bots_nudged`
(`HostRelayConfig::with_bots_nudge`). A room nudges on a new link, question or answer;
progress states don't nudge.

### 11.6 Privacy

Bodies cross the Edge in plaintext and rest in DO SQLite until acked or expired, as chat2
transcripts do today, and the UI says so. End-to-end encryption with per-bot X25519 keys
exchanged at redeem time is a later phase; it needs no new routes, only opaque `text`.

As built, the scrub (`bots/network/scrub.rs`) redacts PEM private-key blocks (an
unterminated one to the end), AWS key ids, GitHub, Slack, `sk-` (OpenAI, Anthropic),
Stripe and Google API keys, JWTs, `Bearer`/`token` values of 20+ characters, URL
credentials, `.env` lines whose name contains secret, token, password, api key, private
key, credential or access key, and quoted JSON/YAML values under such names. Names stay;
values become `[redacted]`. Review drafts are scrubbed before the owner sees them; the
owner's edited text is sent as written.

### 11.7 Groups

First a group is the owner's label over their own connections (a `botGroups` registry
row), which `ask_group` fans out to. Later a `GroupRoom` DO holds shared membership:
joining by one invite connects a bot to every member, with the group's default policy,
which each member can narrow. Not built.

### 11.8 The host engine (as built)

`crates/engine/src/bots/network.rs` and `network/` serve the bots hosted on this device.
`EngineCore` attaches the Edge client (`HttpBotEdge`, a fresh bearer per request) when
the engine has an Edge; tests attach a fake through the `BotEdge` trait. The owner name
peers see is the signed-in profile's name, else its email.

- **Room.** A bot's room id lives in `<store>/bots-network/<botId>.json` with the profile
  last published, beside the bot folders, so it survives a recreated folder. The first
  `CreateBotInvite` or `RedeemBotInvite` mints and publishes it; a rename or pause
  republishes. A room id someone else already claimed is replaced once.
- **Invite.** `CreateBotInvite {botId}` publishes, creates the invite on the Edge and
  writes a `BotInvite` row (never the code). `RedeemBotInvite {botId, code}` publishes,
  calls `connect`, and writes the `BotConnection` row (reply mode Auto, no narrowed scope)
  with a `ConnectionAdded` activity. The inviting host writes its own row from the
  `link added` entry and counts the invite's use.
- **Loop.** One host task wakes on a nudge, on any bot record change (debounced 300 ms)
  and every 60 s. Each pass acts on owner edits, then pulls a room when it was nudged,
  at engine start, or when it has live connections or waiting questions and was last
  pulled a minute ago. Pulls run one at a time, and every handler is idempotent, so a
  replayed entry is harmless. Entries are acked only after their rows are written.
- **A peer's question.** `ask` writes an Incoming `BotExchange` (Delivered) and is acked;
  then, unless replies are Off (declined at once, no run), the host posts `working`,
  creates the connection's peer thread on first use (a hidden chat with
  `BotChat::thread(bot, Peer, Peer{connectionId})`, the bot folder as cwd and the bot
  chat's config with a read-only sandbox, recorded as the connection's `threadChatId`)
  and queues a Run there: `Question from "<bot>" (<owner>'s bot):` and the question, with
  message id `peer-<exchangeId>`. The reply is the turn's last assistant text, once a
  later question started or the session row settled after it. A failed or
  10-minute-long run is declined. The reply is scrubbed, then sent (Auto, `Answered` and
  a `PeerAnswered` activity) or stored as the draft with `awaiting_review` (Review).
  Questions left Delivered or Working by a restart are picked up again, and a question
  already in the thread is not asked twice.
- **Review.** When the owner writes `review` on an awaiting exchange (`reviewBotExchange`),
  the host sends the edited text, or the draft, or declines. An answer the Edge refuses as
  gone (404/409) settles the row as Expired; a network failure retries on the next pass.
- **Revoke.** `revokeRequestedAt` on a connection makes the host call `DELETE
  connections/:cid`, set `revokedAt`, log `ConnectionRevoked` and expire its waiting
  questions. A `link revoked` entry does the same on the other side. A deleted bot's
  live connections are revoked by its old host, which still holds the room file. An
  invite's `revokeRequestedAt` makes the host revoke it on the Edge and set `revokedAt`.
- **Asking.** `ask_bot` publishes if needed, posts `ask`, writes the Outgoing exchange
  (`PeerAsked` activity), and waits for the pull that settles it, pulling itself every
  15 s in case a nudge is lost. An answer marks the calling thread tainted
  (`Taint::mark`, "<bot> answered in this thread").
- **Bounds.** Rows are clamped before every write (answers to `ANSWER_MAX` characters);
  at most `KEEP_EXCHANGES` exchanges and `KEEP_ACTIVITY` activity entries per bot, the
  oldest settled ones going first.

## 12. Cost

Durable Object requests bill 1:1 over HTTP and 20:1 for inbound WebSocket messages, and
duration bills while an object is awake (docs/operations/cloudflare-billing.md).

- **No idle cost.** No per-bot socket or heartbeat: one exchange is about 6–10 billed
  requests and 15–25 rows written, and nothing bills between exchanges.
  As built, one deviation: besides the nudge, the host pulls each room with live
  connections once a minute (§11.8), a safety net for a lost nudge that costs one
  request per networked bot per minute while its host is online (about 1,440 a day).
  Dropping it, or stretching it, leaves delivery to the durable nudge alone.
- **The real cost is the runs.** Every thread syncs through chat2 like any chat. A peer
  question costs about two agent turns; an orchestration costs its own turns plus its
  workers', the same as running those sessions by hand.
- **Search stays local.** Scanning or indexing reads stored snapshots and never holds
  chat2 rooms open.
- **Bounded worst case.** Rate limits per connection, budgets per routine and limits per
  orchestration cap a runaway loop.

Measure the network with `scripts/ops/edge-billing-local.mjs` against two dev engines
before it ships.

## 13. UI

Built (P0):

- **Bot strip:** 30px avatar squares between the sidebar header and the project cards,
  lined up with the project glyphs; hidden when there are no bots. The open bot is ringed
  in its own colour, a corner dot carries its chat's status, a hover shows the name, and a
  right-click opens Open / Settings… / Delete…. Bots are created from the header's `+`
  menu (New project… / New bot…). Project windows have no strip.
- **Bot palette** (create and edit), drawn like the Quick Chat and Add project palettes: a
  frosted header band, the form on the left (name beside a live avatar; twelve robot
  faces, a few symbols or the initial, and a colour; Permissions; instructions), a "Run
  on device" rail on the right (two-line device rows, fixed after creation), and a Cancel
  / Save footer with Delete when editing.
- **Bot conversation:** an ordinary session tab that wears the bot's avatar. A quiet access
  note over the composer ("All projects · reads conversations") opens the bot's settings.
  A new bot opens on a start screen with a 2×2 grid of suggested questions. Bot tool calls
  read as what they did ("Read session · <title>"; "Looked over your work · read 2
  sessions"), and cited `cypher://session/<id>` links open the session. No terminal and no
  dock; the composer asks "Ask <bot>…".

Planned:

- **Capabilities in the palette:** *Work directly*, *Coordinate: start* and *Coordinate:
  steer* as separate switches below Permissions, the coordination switches marked as the
  ones that let the bot act. A bot with *Coordinate* on says so in its access note.
- **Plan card:** steps, projects, devices and dependencies, with Approve / Edit / Cancel.
- **Progress card:** each step's state and session link, elapsed time and Stop; it turns
  into a result card when the orchestration ends.
- **Worker sessions:** "Started by <bot> · <task>" on their row and tab.
- **Botboard** (§13.1): bots move out of the project sidebar into a mode of their own.
  The sidebar shows the current bot's cards, the chat stays in the middle, and a detail
  panel slides in on the right when a card's item is opened. This replaces the built
  strip and the header's New project / New bot menu.
- **Project tasks:** recurring tasks in a project's menu, with their last runs.
- **iPhone:** the bot list, the bot chat, plan approvals and reviews, driven by push
  notifications.

### 13.1 Botboard

Bots and projects are two modes of the window, so project work and bot work never share
the sidebar. The mode switch is the sidebar's logo:

| | Projects mode | Botboard mode |
|---|---|---|
| Logo | **Cypher** ⇅ | **Cypher Botboard** ⇅ (the second word in the bot's accent) |
| Sidebar | Project and session cards, as today; no bot strip | The bot picker, then the current bot's cards |
| Middle | Session tabs | The bot's chat |
| Right | The session dock | The detail panel |
| Header `+` | New project | New bot |

```
Botboard mode
┌ sidebar ───────────┐┌──────── chat ─────────┐┌── detail ──┐
│ Cypher Botboard ⇅  ││                       ││ Fix invoice│
│ [Bob][Rel][Team][+]││  conversation with    ││ rounding   │
│ ┌ Status ────────┐ ││  the bot; plan,       ││ steps,     │
│ ┌ Needs you (2) ─┐ ││  progress and result  ││ sessions,  │
│ ┌ Work ──────────┐ ││  cards inline         ││ Stop       │
│ ┌ Schedule ──────┐ ││                       ││            │
│ ┌ Projects … ────┐ ││  [ Ask Bob…         ] ││        [×] │
└────────────────────┘└───────────────────────┘└────────────┘
```

**Switching.** A click on the logo switches mode; its tooltip names the other one. Each
mode keeps its own state: the bot that was open, the session tabs and their layout. In
Projects mode the logo carries a small bot badge with the number of items that need the
owner across all bots, so attention is visible without bringing bot content back into
the project sidebar. Sessions a bot started stay in their projects, marked "by <bot>".
Project windows have no Botboard.

**The bot picker** heads the Botboard sidebar: a row of avatar squares, the current one
ringed in its colour, each with its Needs-you count or status dot, then `+` for a new
bot. A click switches the whole board to that bot; a right-click opens Open /
Settings… / Delete…. With many bots, the row becomes the current bot with a dropdown
(open question, §17).

**The cards** below it describe the current bot, top to bottom:

| Card | Shows | Opens in the detail panel |
|---|---|---|
| **Status** | Avatar, name, host and its online dot, one status line ("1 task running · 2 need you"), and permission badges (*Reads 3 projects · Works directly · Can start sessions*), the ones that let it act in a distinct colour; a settings button | The bot's settings palette |
| **Needs you** | Plans awaiting approval, workers asking or failed on an approval, tainted actions awaiting approval, and later review drafts | The item with its context and actions: the plan to approve, the question with the worker's recent messages and the bot's suggested answer, the action and why it needs approval |
| **Work** | Running orchestrations with their current step, a step bar and elapsed time; the last few finished ones with a one-line result | The orchestration: every step with project, device, worker session link, state and duration; Stop; the result |
| **Schedule** | Routines and its projects' recurring tasks, in order of next run: name, trigger, next run, last result; today's budget use | The routine or task: trigger, prompt, permissions, run history with links, pause, edit, run now |
| **Projects** | Each granted project with device, running and waiting sessions, and "works directly" or "via sessions" | The project's sessions in scope with status, the bot's worker sessions there, git state |
| **Notebook** | The latest few entries, bot-written ones marked | All entries with their sources, editable and deletable |
| **Activity** | The last few things the bot did | The full audit log, filterable by kind |
| **Network** (P4) | Connections with reply mode and last exchange; pending invites | A connection: its scope, reply mode, rate limits, exchanges, revoke |

A card with nothing to show is not drawn; a read-only bot shows only Status, Projects,
Notebook and Activity. **Needs you** always sits right under Status, so attention items
are never below the fold. A card's header opens its whole list; a row opens that item.

**The chat** is the bot conversation as built: the owner instructs the bot here, and
plan, progress and result cards appear inline. Its access note over the composer opens
the bot's settings. It has no terminal and no session dock.

**The detail panel** slides in from the window's right edge and the chat narrows to make
room. It shows one item in full with its actions, closes with × or Esc, and opening
another row replaces its content; the opened row stays highlighted in the sidebar.

Behaviour:

- Cards update live from the engine (`WatchBotWork`, the routine and notebook rows, the
  audit log); they hold no state of their own. Resolving an item anywhere updates every
  count: the card, the Status line, the picker and the Projects-mode logo badge.
- Settings stay in the palette; no card edits permissions itself.
- Usage meters and embedded transcripts are left out: rows link to the thread or session
  instead.

Phasing: the mode switch, picker, Status and Projects cards replace the built strip
first, since they need nothing new from the engine; Needs you, Work and Activity arrive
with the coordinator (P2), Schedule and Notebook with P3, Network with P4.

## 14. RPC

Each method is one constant plus one `SPECS` entry in `cypher_rpc::methods`.

| Method | Kind | Phase | Notes |
|---|---|---|---|
| `CreateBot` | unary, forwardable | P0 | Runs on the host: folder, bot chat and row in one batch. |
| `WatchBots` | stream | P0 | The registry's bot rows. |
| `BotTool` | unary, local | P0 | The extension's entry point; refusals come back as text for the model. |
| `Mutate updateBot` / `deleteBot` | unary | P0 | LWW edits; delete takes the bot's chats with it (and later its connections and room). |
| `ResolveBotPlan` | unary, forwardable | P2 | Approve, edit or cancel a plan card. |
| `StopBotThread` | unary, forwardable | P2 | Stops an orchestration or routine thread and the sessions it started. |
| `WatchBotWork` | stream, forwardable | P2 | Orchestrations, their steps and sessions, for the progress cards and *Work* pane. |
| `SaveBotRoutine`, `DeleteBotRoutine`, `SaveProjectTask`, `DeleteProjectTask` | unary, forwardable | P3 | Routine and recurring-task definitions (registry rows). |
| `CreateBotInvite` | unary, forwardable | P4 (built) | Runs on the bot's host: `{botId}` → `{inviteId, code, expiresAt}`; publishes the room first. The code is shown once. |
| `RedeemBotInvite` | unary, forwardable | P4 (built) | Runs on the bot's host: `{botId, code}` → the new `BotConnection`. |
| `Mutate revokeBotInvite`, `updateBotConnection`, `revokeBotConnection`, `reviewBotExchange` | unary | P4 (built) | Owner fields on the rows (`revokeRequestedAt`, `replyMode`, `scope`, `review`); the host acts on them. |
| `WatchBotRecords` | stream | built | Connections, invites, exchanges and activity, with the other records. |

As built, `ask_bot` is a `BotTool` call that waits for the answer, so there is no
`BotAsk` stream, and exchanges and reviews ride `WatchBotRecords` and `Mutate`.

## 15. What must be true before each step ships

| Before | Must hold |
|---|---|
| Any second kind of thread | One role-aware run profile; origin stamped by the engine on the chat row; the "transcript text is data" instruction in every bot prompt. |
| *Work directly* | Owner threads only; permission control on with the bot's local checkouts as workspace roots; the taint rule. |
| *Coordinate* | Capabilities checked per call; an audit log of every coordination call; worker sessions limited to the read scope; plan approval; Stop that reaches every started session; limits. |
| Routines and recurring tasks | A non-interactive run mode that denies approvals and settles as failed; budgets; the `paused` kill switch; no `ask_bot` from routines. |
| Bot-written notes | Provenance on every entry and a notebook view to delete them. |
| *Represent* | Peer threads read-only with no notebook, no network tools and no slash commands; scope ∩ connection scope; the outgoing scrub; rate limits. |
| *Coordinate: steer* | The same as *Coordinate*, plus per-session confirmation for sessions the bot did not start. |

## 16. Phases

| Phase | Delivers |
|---|---|
| **P0: local bot** (done) | `bots` rows, the bot chat, the bot spawn mode with tool allowlist, four read tools (bounded scan), scope enforcement, the strip, palette and conversation UI, use from other owner devices. |
| **P1: foundation** | Role-aware run profiles with engine-stamped origins; the untrusted-data prompt; the non-interactive run mode; the FTS index, `session_activity` and `project_git`. |
| **P2: coordinator** | *Work directly*; *Coordinate: start* with `propose_plan`, orchestration threads from a brief, worktree and session tools, worker marking; plan, progress and result cards; Stop; limits; audit log. |
| **P3: memory and routines** | The notebook (owner notes first, then bot notes with provenance); the scheduler; routines and recurring project tasks; the attention inbox ("what needs me"). |
| **P4: network** | `BotRoom`, publish, invites, connections, peer threads, `ask_bot` in Auto mode, exchange states, late answers, the Connections and Activity panes. Built on the Edge and in the engine, with Review mode and the scrub from P5; the panes are the UI track's. |
| **P5: review and control** | Review mode, the outgoing scrub (both built with P4), notifications for reviews, plans and connections; *Coordinate: steer*. |
| **P6: groups, iPhone, E2E** | Owner groups, then shared groups; the iOS bot surfaces; end-to-end encrypted exchanges. |

P2 comes before the network because a coordinator is useful to a single user on day one,
while the network pays off once teammates are on board.

Tests follow the repository's tiers: unit tests for scope, origin and capability checks
(including the "not found" rule, bot-chat exclusion and the taint rule), engine
integration tests with the mock harness driving bot tools and orchestrations, workerd
tests for `BotRoom` (ownership, redeem, revoke, cross-room delivery, rate limits, expiry),
shared frame vectors, and a two-user smoke with one bot each.

As built for P4: `apps/edge/test/workerd/bot-room.workerd.test.ts` (real BotRoom and
DeviceRoom objects behind the route table), `bot-frames.test.ts` and
`crates/proto/src/bot_wire.rs` on the shared vectors, `crates/engine/tests/bot_network.rs`
(two engines as two users through an in-process fake Edge), and `scripts/e2e-bots.sh`
(local `wrangler dev`, two headless engines, `crates/rpc/examples/bot_network_driver.rs`).

## 17. Open questions

- **Plan approval.** Approval first is decided; when does auto-start become available, and
  should it be limited to plans that only start sessions in worktrees?
- **Direct work default.** On for new bots is the proposal; should it instead be off until
  the owner turns it on?
- **Cross-device dependencies.** Which remote does the bot push A's branch to when B is on
  another device: the project's origin, or a Cypher-managed transfer over the device
  relay?
- **Many bots.** The Botboard picker is a row of avatars; past a handful of bots, should
  it become the current bot with a dropdown?
- **Fallback host.** Should a bot have a second host, so work and questions don't wait
  while the only host sleeps? This needs claim and lease logic.
- **Identity shown to peers.** The owner's WorkOS display name, editable per bot, is the
  proposal.
- **Answer provenance.** Should peers see which sessions an answer cited (titles only)?
- **Retention.** Is 7 days right for undelivered questions, and should hidden threads
  expire locally?
