# Native pi RPC harness (2026-08)

## Decision
- Replace the community **pi-acp** adapter path with a **native pi harness**
  (`crates/harness/src/pi/`) that speaks pi's OWN RPC protocol
  (`pi --mode rpc`, strict JSONL over stdio) directly. The pi-acp spec,
  managed-install arm, and `AcpHarness::pi()` are deleted; the registry's Pi
  slot resolves `zeron_harness::pi::PiHarness` instead.
- **Motivation — the ACP adapter's expression ceiling**: pi's RPC mode carries
  capabilities the ACP surface (via pi-acp 0.0.33) could not reach:
  - **extension UI dialogs** — `select`/`confirm`/`input`/`editor` arrive as
    first-class `extension_ui_request`/`extension_ui_response` pairs (the
    adapter flattened them into permission requests or dropped them);
  - **extension commands** — `get_commands` enumerates extension / prompt /
    skill commands directly;
  - **native mid-run steering** — pi `steer` queues into the live turn after
    the current assistant message's tool calls (ACP got turn boundaries only);
  - **real model directory** — `get_available_models` returns pi's configured
    providers/models with reasoning + context-window metadata (the adapter
    advertised a single pass-through `default`).
- Wire is hand-rolled tolerant serde against raw `Value`s (house style, like
  ACP) — NOT the official SDK — so zeron keeps its child-lifecycle hardening
  (StderrTail, SIGTERM→SIGKILL, PATH composition) and shell-script test
  fixtures. **Not JSON-RPC 2.0**: a small line transport
  (`pi/client.rs`) frames on `\n` only (strip trailing `\r`), never on
  Unicode separators — Node `readline` splits on U+2028/U+2029, which are
  valid inside JSON strings.

## Session truth division
- **zeron doc = display/sync truth** — the harness never touches it.
- **pi session file = LLM-context truth.** `pi --mode rpc --session-dir
  <zeron-owned-dir>` (the profile store's `agent-sessions/`) keeps pi's
  sessions in a zeron-owned directory.
- `RunRequest.resume` (engine-injected) carries the pi session file's
  **absolute path**: a present value first sends `switch_session`; a failure
  is a LOUD error — `Done{Errored}` whose message names the failure and the
  path (`pi session resume failed: {err} ({path})`) — never a silent fresh
  session. An absent value means a fresh session pi creates itself.
- `get_state().sessionFile` is reported as `SessionStarted.session_id` and
  `Done.session_id` (the engine's resume bookkeeping keys on it, already
  chat-memory-aware).

## Protocol surface used
- Discovery (short-lived probe processes, killed after): `models()` =
  `get_state` (liveness) → `get_available_models` → map
  `id = "{provider}/{modelId}"` (pi's provider/id convention), `label =
  name`, `description = "{provider} · {n}k context"` (the picker renders it
  on the row's muted subline after the harness name — without the provider,
  the same vendor model served by several providers is indistinguishable),
  and the full reasoning ladder `[Minimal..Max]` only when `model.reasoning`
  is true; `commands()` = `get_commands` (extension / prompt / skill).
- Run (one child per run, cwd = `RunRequest.cwd`): `switch_session` (resume)
  → `set_model` / `set_thinking_level` (best-effort — a rejected set is
  logged, never fatal, like ACP's `set_config_option`) → `get_state` →
  `prompt` (message + inlined `image/*` attachments; the response only means
  acceptance) → the event stream.
- Steering: a mailbox steer while the turn is ACTIVE sends `{"type":"steer"}`
  (pi-native mid-run delivery — after the current assistant message's tool
  calls, before the next LLM call); the NEXT assistant `message_start` emits
  `Steered { prev, next }` before the steered content streams (the same
  boundary point as the ACP harness). A mailbox message arriving while the
  session is PARKED restarts it via `{"type":"prompt", …,
  "streamingBehavior":"steer"}` — **atomic across pi's real state**: an
  idle pi starts a fresh turn; a pi still (or newly) streaming queues the
  message as a steer. The `Steered` boundary fires BEFORE the parked prompt
  is dispatched, so pre-response extension notify/dialog output folds into
  the new turn's segment. A raw `steer` is never sent idle (a parked pi only
  QUEUES steers — it would strand forever) and a plain `prompt` is never
  sent while pi streams (pi REJECTS a prompt without `streamingBehavior` —
  the confirmed parked-session wedge).
- Parked restarts: after `agent_settled` the run PARKS (the child persists
  for the next routed send). Each parked restart RESETS per-turn state for
  the new turn (last text/stopReason, the activity flag, and the progress
  throttle), and a steer pi ACCEPTED but never delivered before a settle
  (the turn ended while the steer was queued inside pi) is retried through
  the same parked restart — never dropped or stranded.
- Interrupt: `{"type":"abort"}`, wait for `agent_settled` → `Done{
  Interrupted}`, escalating SIGTERM → SIGKILL.
- Extension UI bridge: `select` → `UserInputQuestion{options}`,
  `input`/`editor` → free-text `options: []` (prefill ignored — no
  prefilled-text slot in the input bridge), `confirm` →
  `["Confirm", "Cancel"]`; answers map back to `extension_ui_response`
  (`value` / `cancelled` / `confirmed`). `timeout` is pi-side; the client
  keeps no timer.

## Synthesized built-in commands
pi's `get_commands` advertises extension / prompt / skill commands ONLY —
official behavior: built-in TUI commands (`/compact`, `/export-html`, …) are
not listed and, sent as prompt text, would not execute (pi only executes
`get_commands` results via `prompt`). zeron synthesizes the built-ins that
have RPC equivalents so they stay usable from the composer:

- `compact` — `{"type":"compact"}` (+ `customInstructions`), advertised as
  "Compact the conversation context (pi built-in)", hint "custom instructions";
- `export-html` — `{"type":"export_html"}` (+ `outputPath`), advertised as
  "Export the session to an HTML file (pi built-in)", hint "output path".

`commands()` appends these to the `get_commands` probe result with dedup: a
same-name discovered extension/prompt/skill command wins (no synthesized
twin), and synthesized entries land at the tail. Dispatch is harness-side
interception in `run` — after `SessionStarted`, before the normal `prompt`
path, when the prompt is exactly `/{name}` or `/{name} <rest>`:
`/compact`/`/export-html` are sent over RPC and answered with one `TextDelta`
(also Done's `result`) plus an immediate `Done` (no agent stream, so the
no-activity grace never applies; `compaction_start`/`end` stay ignored). A
populated discovery cache listing a same-name command disables interception
(pi handles its own extension); an unpopulated cache still intercepts — the
popup's selections already passed through `commands()` dedup.

`/model`, `/new`, `/resume` and the rest are NOT synthesized: zeron already
has equivalent UI for `/model` (the model picker), and `/new`/`/resume` are
session lifecycle owned by the engine's resume bookkeeping — TUI-exclusive
with no RPC equivalent worth dispatching.

## Event mapping
| pi event | zeron event |
|---|---|
| `message_update.assistantMessageEvent` `text_delta` | `TextDelta` |
| `…` `thinking_delta` | `ReasoningDelta` |
| `…` `*_start`/`*_end`/`toolcall_*` | internal state only |
| `message_start` (assistant, after an accepted steer) | `Steered` boundary |
| parked restart (`prompt` + `streamingBehavior:"steer"` dispatched while `in_turn == false`) | `Steered` boundary emitted at DISPATCH, before the prompt — pre-response notify/dialog output folds into the new segment |
| `message_end` (assistant) | `AssistantMessageCompleted` (journal boundary) |
| `tool_execution_start` | typed `ToolCall` (pi tool-name mapping, below) |
| `tool_execution_end` | `ToolResult` (capped 16KB output text, no diff) |
| `extension_error` | `Error` |
| `agent_settled` | `Done` (status from last assistant `stopReason`: `error`→Errored, `aborted`→Interrupted, else Completed; `result` = last assistant text; `session_id` = sessionFile) |
| `agent_end`/`turn_*`/`queue_update`/`compaction_*`/`auto_retry_*`/`summarization_*`/`bash_execution_update` | ignored |
| child EOF / crash | `Done{Errored}` via the `crash_message` path |

## Extension UI surface policy
pi's `extension_ui_request` methods sort into four classes (a run with NO
agent events at all still terminates — see below):

| class | methods | zeron behavior |
|---|---|---|
| blocking dialogs | `select` / `confirm` / `input` / `editor` | **bridged** — correctness requires the answer (see the UI bridge above) |
| output channel | `notify` | `notifyType: "error"` → `Error` event; `info`/`warning` → `TextDelta` (message fed into Done's `result`; multi-line `\n`-escaped text passes through as-is) |
| structured status | `setStatus` with `statusKey: "zeron.subagents.v1"` | **parsed** — see the Subagent status protocol below |
| transient TUI furniture | `setStatus` (any other key) / `setWidget` / `setTitle` / `set_editor_text` | **deliberately ignored** — transient TUI staging, zeron has its own state surface |
| TUI-only | `custom()` / `setFooter` and friends | no-op in RPC mode (pi itself does nothing with them) |

## Subagent status protocol (`zeron.subagents.v1`)

The pi subagents extension (`extensions/subagents`) publishes a **structured**
live projection through `setStatus`, the one exception to the
TUI-furniture rule above:

- key `zeron.subagents.v1`; value = `JSON.stringify({version: 1, runs: […]})`
  where each run is `{runId, toolCallId?, agent, model?, task, mode, status,
  progress?, startedAt, updatedAt, endedAt?}` (camelCase). `mode` ∈
  `sync|async|message`, `status` ∈ `running|done|error`. Timestamps are epoch
  millis. An empty/blank `statusText` is a CLEAR snapshot.
- **RPC-mode only**: the extension only calls `setStatus` when `ctx.mode ===
  "rpc"`; the TUI keeps its own widget and never displays this JSON.
- Bounded by the publisher (≤32 runs, task ≤500 chars, progress tail ≤8
  lines/4KiB, whole snapshot ≤64KiB, ANSI-stripped) and **re-validated** by
  the harness: wrong version, invalid JSON, oversize snapshot, over-cap
  runs/tasks/progress, or an unknown enum → `warning` + ignore (never
  interrupts the run).
- The harness emits `AgentEvent::SubagentStatus { runs }` for a valid
  snapshot (and an empty list for a clear). The engine treats it as **live
  projection only**: it updates the chat's `Session.subagents` row +
  `updated_at` and stops — it is never journaled, never folded into the
  transcript doc, never bumps lastMessage, and never transitions a parked
  session Working. The GPUI panel merges the projection with the doc's own
  subagent tool parts (async tool results are only launch acks — a missing
  snapshot reads as "Async started", never Done).
- The extension publishes sync start/done/error, async snapshots (the outer
  tool call returns immediately; the snapshot stays `running` until the
  background child finishes), and subagent-to-subagent message activity
  (`mode: "message"`, no `toolCallId`).

## Zeron-hosted child chats (`StartSubagent` bridge, 2026-08)

In Zeron RPC mode the ENGINE can host a subagent's child chat as a **first-class
navigable session**, instead of the extension owning an ephemeral child pi
process. This is the “native pi RPC only” path: a standalone pi TUI keeps the
extension's own `spawnInteractiveSubagent` fallback untouched.

### Ownership & data model

- The parent/child relation is **stored by Zeron** (never assumed from pi): the
  synced `Chat` row carries an additive `child` metadata block — `parentChatId`,
  `parentRunId` (the `zeron.subagents.v1` run id), `toolCallId` (the durable
  link to the parent's transcript part), `agent`, `task`, `mode`, and a
  persisted child agent profile (`systemPrompt` / `tools` / `model` /
  `thinking`). The profile is what later direct turns in the child chat
  re-apply — no arbitrary persisted env maps.
- The messaging **channel is deliberately NOT persisted or synced**: the
  channel root is an absolute host-local path (`/tmp/pi-subagents-messages/...`)
  that is stale after a reboot and outside this sync/ownership boundary. The
  engine keeps it in a LOCAL runtime map (keyed by child chat id) only long
  enough for the initial queued run — registered by `StartSubagent`, consumed
  at first dispatch, bounded (capped, removed on child delete/rollback). Later
  child turns launch `PI_SUBAGENT_ROLE=child` with the persisted profile but
  have NO message channel; the messaging tools then honestly report
  unavailable.
- The `zeron.subagents.v1` projection's per-run `childChatId` (a `SubagentRun`
  field) links a snapshot run to its navigable child chat.
- Child chats are **hidden from the root sidebar/session overview**; they are
  reached only through the parent's Subagents inspector and remain reopenable
  after completion (their pi session file persists like any chat). The
  inspector merges the parent's durable child Chat rows into its aggregation:
  a child row matched by `parentRunId` (or the doc part's `toolCallId`)
  restores `childChatId`/agent/task/mode unconditionally, the child's OWN
  session row is the execution truth (Working/AwaitingInput → Running/Stale,
  Errored → Error, Idle → Done), and a child row with no parent-side entry is
  synthesized — so completed children stay clickable even after the parent's
  snapshot goes empty or the parent restarts.
- Parent delete **cascades**: the child chat rows/docs are tombstoned and live
  child runs interrupted (best-effort — see the deletion tests).

### Bridge surface (local engine IPC, `ws://127.0.0.1:<ipc_port>`)

The harness injects `ZERON_ENGINE_WS_URL` (production `Engine::assemble_runtime`
knows `ipc_port`) and `ZERON_CHAT_ID` into every pi child; discovery processes
never receive a parent id (they have no `RunControls`).

- **`StartSubagent`** (unary, strict bounded params): validates the parent exists
  and is hosted locally, idempotently creates the same-device child chat
  (deterministic lookup by `parentChatId`+`runId`), inherits the parent's
  space/device/cwd/sandbox, creates a Pi-configured titled row carrying the
  child metadata, registers the messaging channel in a LOCAL runtime map, and
  queues the normal durable Run command, then replies `{childChatId}`. An
  idempotent retry of `(parentChatId, runId)` returns the existing child's id
  WITHOUT queueing a second Run (exactly one run command per child). Optional
  `model`/`thinking` are length-bounded. A queue failure rolls the row back —
  no bogus navigable chat.
- **`WatchAgentEvents`** (stream): replayable per-chat agent events (journal
  replay after `afterSeq`, then live) — the parent extension observes the
  child's terminal `done`/result.
- Interrupt/abort/timeout teardown queues a normal child `Interrupt` command
  (`QueueCommand`), never an extension-owned kill.

### Child run semantics

The engine's pi harness launches the child run with child-agent semantics from
the persisted profile: `--append-system-prompt` (persisted system prompt),
`--tools` = agent allowlist ∪ `send_message`/`read_inbox`/`reply_message`,
`--model`/`--thinking` preserved, and env `PI_SUBAGENT_ROLE=child` +
`PI_SUBAGENT_CHANNEL_ROOT`/`PI_SUBAGENT_RUN_ID`/`PI_SUBAGENT_AGENT`/
`PI_SUBAGENT_CHILD_INDEX` (the messaging channel identity) — the channel env
applies ONLY to the initial queued run (it comes from the engine-local map);
later child turns keep the profile but have no channel and the messaging tools
report unavailable. The run flows through the normal SessionsEngine (journal,
doc fold, status lifecycle, warm parked sessions, resume) — nothing is
special-cased.

### Navigation & status ownership

- Clicking an Inspector row whose run carries a child chat id closes the
  popover and calls the normal `AppState::select_chat(Some(childId))`; the
  Shell's existing `NavHistory` observation records the switch, so Back returns
  to the parent session (no second transcript view). The inspector has real
  focused-row keyboard navigation (Up/Down move the tracked active row,
  Enter/Space open it, Escape closes); rows themselves are click targets only.
- The child Chat/session is the execution truth; the parent's `SubagentStatus`
  snapshot remains a summary projection. The durable child rows are merged
  into the inspector aggregation (see Ownership above), so a completed child
  stays reopenable even after the parent snapshot went empty or the parent
  restarted, and the child's own session status (Working/AwaitingInput →
  Running/Stale, Errored → Error, Idle → Done) is what the row displays when
  available.
- The engine never resurrects ghosts: the parent status board publishes
  `Running` only once the child chat is real (the `StartSubagent` reply), and
  settled/errored runs terminalize normally.

`message`-mode pooled activities (subagent-to-subagent routing) remain on the
extension-owned spawn path in this first implementation — they are not
engine-hosted children.

## No-activity termination
A `prompt` accepted by pi but followed by **zero agent-lifecycle events**
must not leave the run "Working" forever (an extension command whose handler
only notifies produces exactly that). After the prompt response, the harness
arms a grace timer (`no_activity_grace`, default 2s, tunable via
`with_no_activity_grace`); the first **lifecycle** event
(`agent_start`/`turn_*`/`message_*`/`tool_execution_*`/`agent_end`/
`agent_settled`) disarms it. Informational events do NOT count —
live-verified against pi 0.84.1: `set_model`/`set_thinking_level` emit a
`thinking_level_changed` event and can provoke `extension_error`s; counting
those as activity is what hung real `/subagents` runs. If the timer fires
while no lifecycle event has arrived, the run ends `Done{Completed}` whose
`result` is the accumulated notify text (so a notify-only command still
carries its output), and the child is reaped immediately. sendMessage work
arriving after that fire is dropped — a documented degradation. Each parked
restart re-arms a FRESH grace timer only once its prompt is ACCEPTED (the
previous turn's sleep may already have elapsed and must not fire during the
prompt's preflight); lifecycle events that land before the response disarm it
via `agent_started`, and a genuinely inert (notify-only) parked turn still
terminates with its own notify text.

- **Segment semantics** (engine consumption is the source of truth; ACP is the
  reference): the doc fold splits entries only on `Steered` (and clears on
  `SessionStarted`) — `AssistantMessageCompleted` is journal-only, matching
  the ACP turn-boundary markers. A pi turn has multiple assistant messages
  (LLM→tool→LLM); each `message_end` emits its `AssistantMessageCompleted`,
  and the whole turn folds into one doc entry until a steer boundary or the
  terminal `Done` — identical to an ACP turn.
- **Tool-name mapping** (`pi_typed_call`): pi's built-in set is
  `bash`/`read`/`write`/`edit`/`grep`/`find`/`ls` → `Exec`/`ReadFile`/
  `WriteFile`/`EditFile`/`Search`/`Glob`/`Search`, sharing the ACP normalizer's
  `cap_text` and 16KB cap; anything else (extension/MCP tools) falls through
  to `ToolCall::Unknown` with the raw args.

## Executable resolution
`PI_EXECUTABLE` override → PATH → login-shell PATH (`shell_env.rs`) → npm
global bins + node-version-manager bins (the shared `acp::find_on_paths` +
`npm_global_bins`). `installed()` = a resolution hit. Discovery and run spawn
the resolved `pi` with `--mode rpc --session-dir`.

## Citations
pi RPC protocol doc (`docs/rpc.md` in the pi package), pi 0.84.1 source
(`rpc-mode.js` command/response/extension-UI handling, `agent-session.js`
event emission order, `pi-agent-core` agent-loop tool/message ordering).
