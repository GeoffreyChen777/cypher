# Agent bots

Status: **P0 and P1 implemented; P3's engine side built.** P0: local, read-only bots
(definition, scope, the bot chat and its read tools, the palette). P1: engine-stamped
origins, the role-aware run profile, per-origin prompts with the untrusted-data rule, slash
commands by origin, the unattended run mode, the search index, `session_activity` and
`project_git`. P3 (engine): the notebook tools, the scheduler, routines and recurring
project tasks (§7, §8). P2 (engine): *Work directly*, plans, orchestration threads,
worker sessions, Stop, limits, approvals and the activity log (§4.3, §4.4, §6.7, §10.2,
§10.3). The first Botboard slice (§13.1: mode switch, picker, Status and
Projects cards, detail panel) is built. The rest is design; §16 is the build order. Where
the built code differs from the full design, the section says so.

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

As built (P2): the taint set lives in memory (`bots/taint.rs`); the network track marks a
thread when `ask_bot` returns a peer's answer. In a tainted thread every acting
coordination call (`create_worktree`, `start_session`, `answer_session`, `steer_session`,
`stop_session`) and every `bash`, `write`, `edit` or `request_permission` call writes a
`botApprovals` row with the action in plain words and the reason, and waits up to 10
minutes for the owner's decision (`Mutate resolveBotApproval`). Declined or unanswered,
nothing happens and the model is told so. Reads are not gated. The work host clears a
thread's taint once its run settles after the taint was set. Approvals are also how a
thread with *Coordinate: steer* answers, steers or stops a session its work didn't start.

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
marker. `bots/profile.rs` derives the run profile from (chat, bot row, origin).

As built (P2), `bots/work.rs`:

- **Tools.** Routine and peer threads get the read tools only. An owner thread with *Work
  directly* adds Pi's `bash`, `read`, `write`, `edit`, `grep`, `find`, `ls` and
  `request_permission`. With *Coordinate*, the bot chat adds `propose_plan`, `list_work`
  and `get_work`, and an orchestration thread adds every other coordination tool (§10.3).
- **Roots.** The paths of the bot's granted spaces on this device, canonical. The run gets
  them as `CYPHER_BOT_WORKSPACE_ROOTS` (JSON); the bot tools extension writes each as the
  permission extension's `cypher-permissions.root` entry for the chat in its own
  `session_start`, and removes roots the engine no longer grants. Pi loads `-e`
  extensions before the Runtime's and runs their handlers in that order, so the entries
  are on the branch when the permission extension reloads its roots, on a chat's first
  run too. Checked on a real Pi run (Runtime 1.1.0.8): a new bot chat's first `write` and
  `bash` into its project ran without any approval, while a write outside the roots
  needed one.
- **Level.** The engine decides a bot thread's permission level whatever its chat row
  says: workspace-write for an owner thread with *Work directly* and at least one root,
  read-only for every other bot thread (`profile::run_sandbox`, applied at dispatch and to
  the steer path's launch config, so a parked run isn't retired over the row's level).
- **The gate.** Pi's own tools run inside Pi, so before each call the bot tools extension
  asks the engine (`BotTool` with the engine-only tool `direct_tool`) and blocks the call
  unless the answer is `allow`; it fails closed. The engine refuses when the bot no longer
  has *Work directly*, when permission control is off on this device (it is off by
  default) or Landstrip can't enforce there, and applies the taint rule (§4.3).
- **Limits of the approach.** The bot's folder is inside the profile store, which
  permission control protects as Cypher's own data, so a shell started there prints a
  harmless `getcwd` warning (the prompt says so), and a checkout under `~/.cypher*` can't
  be worked in directly.

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

### 6.7 As built (P2)

- **The plan** is a `botWork` registry row (`cypher_proto::BotWork`). `propose_plan`
  writes it as proposed, with the bot chat and the tool call that draws its card (the
  `BotTool` RPC passes Pi's tool call id), the steps (ids 1 to n, at most 10, projects
  resolved within the scope) and the owner's newest message in the bot chat as the
  instruction. Its answer tells the model to stop.
- **Who writes what.** Owner devices write only `decision` (`Mutate resolveBotPlan`) and
  `stopRequestedAt` (`Mutate stopBotWork`); the host writes the rest, always leaving those
  two fields alone. Every text is clamped before it is written.
- **The work host** (`bots/work/host.rs`) runs on the bot's host. It reconciles on every
  change of the bot records, bot rows and session rows, and every 5 seconds:

  ```text
  proposed ── decision cancel, or Stop ───────────────────────────► cancelled
     │ approved, bot not paused, Coordinate on, fewer than 3 running
     ▼
  running ── finish_work ──────────────────────────────────────────► done | failed
     ├─ the thread's run settles without finish_work ──────────────► done (its last
     │                                                                message) | failed
     ├─ Stop: Interrupt the thread and its sessions that still run,
     │  skip the steps not done ───────────────────────────────────► stopped
     └─ a worker waits on an answer: its step is asking, then running again
  ```

  Starting means writing the item running first, then a hidden thread chat (marker
  `{origin: owner, thread: {kind: work, workId}}`, the bot's folder, the bot chat's
  config, the plan's title), then queueing its first run, the brief, under a message id
  fixed per item. After a restart the same pass repairs what a crash left: a running item
  whose thread row or first run is missing gets them. It keeps 40 finished items, 200
  activity lines and 100 resolved approvals per bot, deleting the oldest (with a pruned
  item's thread chat), and expires approvals nobody waits for any more.
- **The thread runs unattended** (§8): it is hidden, so a permission prompt there fails
  the work with the reason instead of stalling where nobody looks. Its worker sessions are
  attended: their questions reach the thread through `wait_for_session` and the owner
  through *Needs you*. A thread may answer a question but never a permission prompt;
  only the owner approves a session's commands.
- **The brief** (`bots/work/text.rs`) carries the plan with step ids, projects and
  devices, the instruction verbatim in quotes, and what the thread may do in plain words.
  Notebook entries join it with the memory track.
- **Sessions.** Everything a thread does to a session is a durable command
  (`DocHost::queue_command`: Run, Steer, Interrupt, RespondInput), the same for a session
  on this device and one on another owner device. A worker is an ordinary chat in its
  project (the `createChat` path), titled with the step, at workspace-write, on the bot
  chat's model unless the call names one; its step records it before its run is queued. A
  session's cwd is its project's folder or a worktree the same work created there (read
  back from the `worktreeCreated` activity line). The plan's projects bound where it may
  work. "Started by" marking reads `BotRecords::started_by` instead of a chat-row field.
- **Limits.** 3 running orchestrations per bot (more approved plans wait for a slot), 8
  sessions per orchestration, 8 per step, 10 steps per plan. The bot's `paused` flag keeps
  approved plans waiting and refuses new worktrees and sessions. Turn and time budgets
  are not built.
- **The activity log** gets a `botActivity` row for each proposal, start, cancel, finish,
  failure, Stop, worktree, session started, answered, steered or stopped, and approval
  asked.

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

As built (P3, engine): a note is a `botNotes` registry row (`BotNote`), not a file, so it
syncs to every owner device like the bot row. The owner writes with `Mutate saveBotNote`
(source `owner`) and deletes with `deleteBotNote`; the bot writes with `write_note`, which
stamps `source: {kind: "bot", chatId, title}` naming the thread it was written in (the bot
chat, an orchestration thread or a routine thread). Who may do what is one function,
`bots::memory::access`, checked on every call:

| Thread | `read_notes` | `write_note` |
|---|---|---|
| Bot chat, orchestration thread (owner) | yes | yes |
| Routine thread | yes | only when its routine's `writeNotes` is set |
| Peer thread | no | no |

A note is at most 1,000 characters (longer ones are cut, and the answer says so); an
empty one is refused. Each write adds a `noteWritten` activity entry. The prompt of owner
and routine threads carries a "Your notebook:" section with the newest 40 notes
(`NOTES_IN_PROMPT`), each with its source and date, and says that `read_notes {query?}`
finds the rest; peer threads get no section. A routine thread's section says whether it
may write.

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

As built (P3, engine). Definitions are registry rows: `botRoutines` (`BotRoutine`) and
`projectTasks` (`ProjectTask`). Owner devices write the definition and `runRequestedAt`
("Run now"); the host writes only `threadChatId`, `lastRun`, `nextRunAt`, `runsToday` and
`runsDay` (a routine) or `lastRun` and `nextRunAt` (a task). A schedule is `{kind:
"daily", time: "HH:MM", days?: [1–7]}`, `{kind: "interval", minutes ≥ 15}` or `{kind:
"manual"}`, in the host's local time.

**The scheduler** (`bots/memory/host.rs`, rules in `bots/memory/schedule.rs`) runs the
routines of bots hosted on this device and the tasks of spaces on this device. One pass at
a time; it wakes every 30 s, on record and bot row changes, when a tool saves a job, and,
while a run it started is going or a post waits, on session status changes. Per job:

- `nextRunAt` is written when missing or stale: a held time stays while it is still one of
  the schedule's slots and no slot comes before it; otherwise it becomes the first slot
  after now. Manual and disabled jobs have none.
- A job is due when `nextRunAt` has passed, or `runRequestedAt` is newer than the last
  run's start (a disabled job still runs when asked).
- **Missed runs:** a slot the device slept through stays due, runs once on wake, and the
  next run is planned from now, so the rest are skipped.
- **Never twice:** while the last run is `running`, a slot that comes due is skipped and a
  request waits for the run to end.
- **Pause and budget:** a due routine of a paused bot, or one that already ran
  `maxRunsPerDay` times today (`runsToday` on the local date `runsDay`), gets `lastRun`
  `skipped` with the reason ("Skipped: the bot is paused."); a request is skipped the same
  way. Tasks have neither.
- **Delivery survives restarts:** the run is recorded (`lastRun {at, outcome: running,
  chatId}`) before its Run command is queued in the chat's durable command ledger. A pass
  settles a running record from that command and the session: a command still pending is
  waiting; a rejected or expired one failed ("The run couldn't start: …"); once applied, a
  working session is waiting and a settled one ended. No command 60 s after the record
  means it never started. After a restart the next pass settles whatever the old engine
  left running.
- **Settling:** a run ends `done` with its last answer as `summary` (clipped to 1,000
  characters) or `failed` with the best reason: the unattended stop's reason, the
  command's, the transcript's last error, or "The run was stopped before it finished."
  for a run interrupted mid-answer or that ended with no answer at all.

**Routine runs.** Each routine has one hidden thread, created on its first run: a chat on
this device marked `{botId, origin: "routine", thread: {kind: "routine", routineId}}`,
titled with the routine's name, in the bot's folder, with the bot chat's config. The Run's
prompt is a header and the routine's prompt: `Routine "<name>" · <Weekday Month D, YYYY,
HH:MM>`, a blank line, then the prompt. The run is unattended (P1). When it settles, the
host writes `lastRun`, adds a `routineRan` activity entry, and **posts the output into the
bot chat**: a complete assistant entry (id `routine-<routineId>-<run start ms>`, so it is
posted once) with one `post` part, written straight into the doc. It starts no turn. If
the bot chat is in the middle of a turn, the post waits until that turn settles. The post
also sets the bot chat's preview to "<name>: <output>" and bumps its activity, so it shows
unread. A failed run posts its reason the same way; failed routines are listed by
`BotRecords::needs_you`.

The `post` part (`cypher_doc::MessagePart::Post`) is additive: `{id, kind: "post", text,
from}`, with the body under `text` so readers that predate the kind show it as a text
part. `from` is a `BotPost`: `{kind: "routine", routineId, name, outcome: "done" |
"failed", runAt, threadChatId?}`; a kind from a newer engine reads as `unknown`. The
desktop transcript renders the body like text for now; the Botboard draws a card with the
routine's name, the time, a failed badge and a link to the thread.

**Recurring task runs.** On the space's device each run creates a fresh session in the
project (`createChat` with the space), titled "<task name> · <Mon D>", records `lastRun`,
then queues a Run with the task's prompt and model. With `worktree` set, the Run carries a
`WorktreeSpec` (the space's folder, its current branch as the base), so the host creates
the worktree at drain time exactly as the composer's isolated sessions do. The agent's
copy of the prompt says the run is unattended. On settle, `lastRun` is updated and every
bot that sees the project gets a `taskRan` activity entry (only the task's `createdBy` bot
when set). Task sessions are ordinary sessions: they show in their project, and a
failure surfaces like any errored session.

**Unattended runs.** `bots::profile::is_unattended(workspace, chat)` covers routine
threads and a task session while the scheduled run it was started for is `running`
(the owner's later messages there run attended). The engine hands every unattended stop
to its listeners (`SessionsEngine::add_unattended_listener`); the scheduler's records the
reason and settles the run `failed` with it.

**Notifications.** There is no new notification path. A failed routine run raises
attention through its post (unread bot chat on every device) and Needs you; a failed task
run is an errored session in its project, which notifies like any other.

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
  `UnattendedStop` (chat id, reason) to its unattended-stop listeners; the scheduler's
  settles the routine or task run as failed with that reason (§8).
- **Coordination (built in P2).** `bots/work.rs` decides each thread's extra tools, its
  workspace roots and its permission level, answers the direct-tool gate, asks for
  approvals and writes the activity log (§4.3, §4.4). `bots/work/coordinate.rs` holds the
  coordination tools and `bots/work/host.rs` the work host, one loop per engine started
  with the other host loops (§6.7). Orchestration threads run unattended like routine
  threads; their worker sessions don't. `Bots::set_pi_runtime` hands the work track the
  Pi runtime, whose permission settings and Landstrip status the gate reads (the status
  is cached for 10 minutes, since `doctor` spawns a process).
- **Scheduler (built in P3).** One host loop (`bots/memory/host.rs`) runs the routines of
  the bots hosted here and the recurring tasks of this device's spaces. All of its state
  is in the `botRoutines` and `projectTasks` rows and the chats' command ledgers, so a
  restart loses nothing: the next pass settles runs the old engine left running. Its
  rules are in §8.

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

Pi's own tools (`bash`, `read`, `write`, `edit`, `grep`, `find`, `ls`, and
`request_permission`) under permission control, with the bot's local scoped checkouts as
workspace roots and the engine's permission level; the engine checks each call first
(§4.4). Built.

### 10.3 Coordination (owner threads with *Coordinate*)

All built. The bot chat gets `propose_plan`, `list_work` and `get_work`; an orchestration
thread gets the rest (§6.7).

| Tool | Does |
|---|---|
| `propose_plan {title, summary?, steps: [{title, project?, detail?}]}` | Writes the plan; its card waits for the owner's approval, which starts an orchestration thread. Bot chat only. |
| `list_work`, `get_work {work_id}` | The bot's plans with state and progress; one plan's steps, their sessions with status and links, and its result. |
| `create_worktree {project, branch?}` | A new `cypher/<name>` worktree in one of the plan's projects, off `branch` (default HEAD), on that project's device: locally through the engine's repos, on another device through its forwardable `CreateWorktree` within 30 s. Returns the path. |
| `start_session {step, project, prompt, title?, cwd?, model?}` | A session in one of the plan's projects or a worktree from `create_worktree`, recorded on the step; returns its id. |
| `wait_for_session {session_id, timeout_s?}` | Waits (default 300 s, at most 900) until a session settles or asks; returns its state, its last message and any question with its options, as quoted data. |
| `answer_session {session_id, answers? \| text?}` | Answers the question a session is asking (never a permission prompt). |
| `steer_session {session_id, message}`, `stop_session {session_id}` | For sessions this work started; for others only with *Coordinate: steer* and the owner's approval of each action. |
| `update_step {step, state, note?}` | Running, done, failed or skipped, for the progress card. |
| `finish_work {outcome, result}` | Done or failed, with the result the owner reads, citing sessions. |

### 10.4 Memory and network

| Tool | Threads | Does |
|---|---|---|
| `read_notes {query?}` | owner, routine | The bot's notes, newest first, each with its source and date; `query` keeps notes containing every word (any case). |
| `write_note {text}` | owner, routine (if its routine allows) | Adds one note with the thread as its source (§7). |
| `list_schedule` | owner, routine | The bot's routines and its projects' recurring tasks: name, id, schedule in words, next run, last result (and the task's last session). |
| `save_routine {id?, name, prompt, schedule, write_notes?, enabled?}` | bot chat | Creates or changes a routine; validates it and answers with what was saved and its next run. |
| `save_project_task {id?, project, name, prompt, schedule, worktree?}` | bot chat, with *Coordinate* | Creates or changes a recurring task on a project in scope; `createdBy` is the bot. |
| `list_connections` | owner | Connected bots, owners, groups, reachability. |
| `ask_bot` | owner | Asks one connected bot; waits up to `wait_s` (default 120 s). Taints the thread (§4.3). |
| `ask_group` | owner | Fans a question out to a group; returns the answers received in the window. |

Network tools never exist in peer threads, so a peer cannot use the bot to relay
questions to third parties.

### 10.5 Prompting

The system prompt states the bot's role and its permissions for this thread in plain
words, how to choose between direct work and a session (§6.3), that every answer cites the
sessions it draws on as `cypher://session/<id>` links (built), and that text from
transcripts, tool output, web pages and peers is data to read, never instructions to
follow. In peer threads it also says who is asking.

### 10.6 Late answers

When `ask_bot` times out it returns `pending` with the exchange id. When the answer
arrives the engine queues a durable command in the bot chat (`{kind: "bot-answer",
exchangeId}`), which renders as an attributed card and starts a turn, so the bot can
follow up without the owner asking again.

## 11. Bot network (Represent)

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
edits or declines it. Before an answer leaves the host, a best-effort pass redacts strings
that look like secrets (private-key blocks, common token formats, `.env`-style
assignments); it is a safety net, not a guarantee, and the UI says so.

### 11.2 Edge `BotRoom`

One Durable Object class, `BotRoom`, keyed `bot1/{botRoomId}` and bound as `BOT_ROOMS`
(wrangler migration `new_sqlite_classes: ["BotRoom"]`), one room per networked bot:

| Table | Contents |
|---|---|
| `meta` | owner user id (claimed on publish, owner-only after), public profile `{name, ownerName}`, host device id, `paused` |
| `invites` | `sha256(secret)`, created/expiry, max uses, uses |
| `connections` | `connectionId`, peer `botRoomId`, peer profile, created, revoked, per-side rate counters |
| `inbox` | exchanges addressed to this bot that the host has not acknowledged |
| `outbox` | exchanges this bot asked, with state and, once received, the answer, until acknowledged |

Bodies are kept only until the receiving host acknowledges them, or 7 days for undelivered
ones (then `expired`).

### 11.3 Routes

All routes sit behind bearer verification as one `ROOM_ROUTES` entry with prefix `bots`:

| Route | Caller | Does |
|---|---|---|
| `POST /bots/:id/publish` | owner | Claims the room and sets the public profile. |
| `GET /bots/:id/inbox` | owner's host | Pulls undelivered inbox and outbox entries. |
| `POST /bots/:id/invites` | owner | Creates an invite; returns `code = <botRoomId>.<secret>` once. |
| `DELETE /bots/:id/invites/:inviteId` | owner | Revokes an unused invite. |
| `POST /bots/:id/connect` | owner of `:id` | Redeems a peer's invite code on behalf of bot `:id`. |
| `DELETE /bots/:id/connections/:cid` | owner | Revokes a connection on both sides. |
| `POST /bots/:id/ask` | owner of `:id` | Sends a question on one of `:id`'s connections. |
| `POST /bots/:id/answer` | owner of `:id` | Answers (or declines) an exchange in `:id`'s inbox. |
| `POST /bots/:id/ack` | owner of `:id` | Acknowledges delivered entries. |

### 11.4 Trust chain

A caller can only act through a bot it owns: every route checks the room's `owner`, the
claim-then-owner-only rule ChatRoom and DeviceRoom use. Cross-user traffic moves **DO to
DO** over internal paths the Worker never routes: `ask` checks the connection and rate
limits on both sides before queueing; `connect` redeems the hashed secret in the peer room;
revocation deletes both sides. Neither user's bearer reaches the other's room, and a peer
cannot address a room it is not connected to.

### 11.5 Delivery and wire shapes

Bots open no socket of their own. A BotRoom that queues an entry sends the host's
DeviceRoom a durable nudge, and the host pulls with `GET /bots/:id/inbox`; entries stay
until acked after the engine has appended them to the peer thread. Exchange state flows
back as `queued → delivered → working → (awaiting_review) → answered | declined |
expired`.

```
ask      {t:"ask", exchangeId, connectionId, text, sentAt}
answer   {t:"answer", exchangeId, connectionId, status:"answered"|"declined"|"expired", text?, answeredAt}
state    {t:"state", exchangeId, state}
link     {t:"link", connectionId, op:"added"|"revoked", peer?:{botRoomId, name, ownerName}}
```

The codec is mirrored in Rust and listed in `protocol/README.md` with shared vectors.
Limits: question ≤ 8 KiB, answer ≤ 64 KiB; per connection 30 questions an hour, 200 a
day, 3 in flight.

### 11.6 Privacy

Bodies cross the Edge in plaintext and rest in DO SQLite until acked or expired, as chat2
transcripts do today, and the UI says so. End-to-end encryption with per-bot X25519 keys
exchanged at redeem time is a later phase; it needs no new routes, only opaque `text`.

### 11.7 Groups

First a group is the owner's label over their own connections (a `botGroups` registry
row), which `ask_group` fans out to. Later a `GroupRoom` DO holds shared membership:
joining by one invite connects a bot to every member, with the group's default policy,
which each member can narrow.

## 12. Cost

Durable Object requests bill 1:1 over HTTP and 20:1 for inbound WebSocket messages, and
duration bills while an object is awake (docs/operations/cloudflare-billing.md).

- **No idle cost.** No per-bot socket or heartbeat: one exchange is about 6–10 billed
  requests and 15–25 rows written, and nothing bills between exchanges.
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

- Cards update live from the engine (`WatchBotRecords`: work, routines, notes, the
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
| `BotTool` | unary, local | P0 | The extension's entry point; refusals come back as text for the model. Carries the Pi tool call's id (P2), and serves the engine-only `direct_tool` gate. |
| `Mutate updateBot` / `deleteBot` | unary | P0 | LWW edits; delete takes the bot's chats with it (and later its connections and room). |
| `Mutate resolveBotPlan` / `stopBotWork` / `resolveBotApproval` | unary | P2 | An owner device writes the control field (`decision`, `stopRequestedAt`, an approval's `decision`); the registry syncs it to the bot's host, whose work host acts on it. Built. |
| `WatchBotRecords` | stream | P2 | Every bot record (work, activity, approvals, notes, routines, network), for the cards and panes. Built. |
| `Mutate saveBotNote` / `deleteBotNote` | unary | P3 | Owner notes (source `owner`) and deleting any note. |
| `Mutate saveBotRoutine` / `deleteBotRoutine` / `runBotRoutine` | unary | P3 | A routine's definition (the host's fields are left alone), and "Run now" (`runRequestedAt`). |
| `Mutate saveProjectTask` / `deleteProjectTask` / `runProjectTask` | unary | P3 | A recurring task's definition, and "Run now". |
| `Mutate setBotPaused` | unary | P3 | The kill switch: due routines are recorded as skipped. |
| `BotAsk` | stream, local | P4 | `ask_bot` / `ask_group`: exchange states until answered or `wait_s`. |
| `CreateBotInvite`, `RevokeBotInvite`, `RedeemBotInvite` | unary, forwardable | P4 | |
| `UpdateBotConnection`, `RevokeBotConnection` | unary, forwardable | P4 | Reply mode, connection scope, rate limits. |
| `WatchBotActivity`, `ResolveBotReview` | stream / unary, forwardable | P5 | Exchanges and review drafts. |

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
| **P2: coordinator** (engine built) | *Work directly*; *Coordinate: start* with `propose_plan`, orchestration threads from a brief, worktree and session tools, worker marking; plan, progress and result cards; Stop; limits; audit log. Built in the engine: all of it but the cards, plus approvals for tainted threads and *Coordinate: steer*'s per-action approval. Not built: turn and time budgets, plan editing. |
| **P3: memory and routines** (engine built) | The notebook (owner notes first, then bot notes with provenance); the scheduler; routines and recurring project tasks; the attention inbox ("what needs me"). Built in the engine: the notebook and schedule tools, the scheduler, routine threads and their posts in the bot chat, task sessions, unattended task runs. Left: the Schedule and Notebook cards and the post card in the UI; event triggers. |
| **P4: network** | `BotRoom`, publish, invites, connections, peer threads, `ask_bot` in Auto mode, exchange states, late answers, the Connections and Activity panes. |
| **P5: review and control** | Review mode, the outgoing scrub, notifications for reviews, plans and connections; *Coordinate: steer*. |
| **P6: groups, iPhone, E2E** | Owner groups, then shared groups; the iOS bot surfaces; end-to-end encrypted exchanges. |

P2 comes before the network because a coordinator is useful to a single user on day one,
while the network pays off once teammates are on board.

Tests follow the repository's tiers: unit tests for scope, origin and capability checks
(including the "not found" rule, bot-chat exclusion and the taint rule), engine
integration tests with the mock harness driving bot tools and orchestrations, workerd
tests for `BotRoom` (ownership, redeem, revoke, cross-room delivery, rate limits, expiry),
shared frame vectors, and `scripts/e2e-smoke.sh` extended to two users with one bot each.

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
