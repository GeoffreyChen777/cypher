# Former Upstream Feature Inventory

This document inventories the product features and meaningful correctness work
added to the former upstream repository after Cypher diverged from it.

It is an audit document, not a merge plan. Cypher has deliberately diverged in
its harness architecture, child-chat model, UI, branding, release
infrastructure, authentication, Side Chat, `@Session`, and Session Fork. Do not
use this list as justification for merging `upstream/main` or bulk
cherry-picking it.

## Current implementation status

The P0 and P1 work described by this inventory has now been implemented
against Cypher's Rust/gpui/Loro architecture. The upstream commit IDs remain
historical references; they are not commits to merge.

| Area | Status | Cypher evidence |
| --- | --- | --- |
| P0 data integrity and crash recovery | **Complete** | Chat2 frontier/cursor/gap repair (`b749a54`), Registry cursor/orphan protection (`1427757`), dead-command recovery (`d6bec99`), retry identity (`aa229a2`), future-config decoding (`3bee7b3`), and upload staging guard (`b828a6d`). |
| P1 durable delivery | **Complete** | Durable worktrees (`2451f0d`, `33979ae`), queue-first attachments and seal wake-up (`9c2b305`, `9439123`), delivery state and retry UI (`9448d2f`, `2bcbc37`), parallel transfer progress (`4add0f2`), indexed file search (`24cd7b9`), and session cycling (`b7d9f88`). |
| Batch 3 diff reconcile churn | **Equivalent and regression-covered** | Reconcile identity memoization, serialized passes, orphan grace, blocking watcher setup, debounce, and checksum suppression are present in `crates/engine/src/diff_sync.rs`; see `crates/engine/tests/diff_sync_churn.rs`. |
| Batch 3 Composer/Transcript checks | **Mostly equivalent; selective fixes landed** | Composer wheel clamping, stream anchoring, viewport/runway handling, entry copy, and streaming row tests are Cypher-native. Soft-wrap decoration ranges and duplicate Codex auth-tab suppression are covered by current fixes/tests. |
| Pull-first HTTPS/reconnect | **In progress — first transport batch landed** | Edge Chat2/Registry pull-push twins, Bearer-only HTTP requests, local-first Rust clients, iOS HTTP polling, and bounded response handling are implemented; cross-device live Edge rollout tests remain. |
| Other P2 architecture projects | **Not started** | Native Claude/Codex, PR status, custom paths, and other major projects remain separate design work. |

The detailed P0/P1 sections below preserve the original audit rationale and
acceptance criteria. Where their older prose says that a vulnerability is
still present or that a port is pending, the status table above is
authoritative for the current tree.

## Current Cypher product decision

This section is the authoritative product decision for the inventory below. It
was written after comparing the upstream changes with the current Cypher
architecture and implementation, rather than treating the upstream commit
list as a merge queue.

### Terminology and hard boundary

“Bring into Cypher” means **manually reimplement the useful invariant or
behavior against Cypher's current Rust/gpui/Loro architecture**. It does not
mean merging `upstream/main`, cherry-picking the listed commits, or copying
upstream files. The former upstream is a Zeron TypeScript/Electron codebase;
Cypher has different ownership, persistence, transport, UI, and harness
boundaries.

The following are already deliberate Cypher architecture choices and must not
be replaced by upstream equivalents:

- native Pi RPC and the Pi-first harness model;
- durable parent/child Chats and exactly-once `StartSubagent`;
- Side Chat;
- `@Session` references and the mixed session/file picker;
- Session Fork;
- Loro session documents and the Cypher command journal;
- Cypher's Rust engine, gpui UI, profile-scoped attachments, Edge/ChatRoom
  protocol, DeviceRoom relay, and release/branding infrastructure.

### Executive decision

| Decision | Meaning | Main contents |
| --- | --- | --- |
| **Completed — P0** | The P0 correctness and recovery ports are implemented and covered by Cypher tests. | Chat2 cursor/frontier/gap handling; Registry cursor integrity and orphan-sweep gating; dead-command recovery and retry identity; forward-compatible config decoding; upload staging mtime guard. |
| **Completed — P1** | The P1 durability and delivery work is implemented against the command journal and current UI. | Durable worktree specification; queue-first attachments and real progress; explicit Failed/Retry/outbox delivery; indexed file search; Ctrl+Tab navigation. |
| **Batch 3 — verified / ported selectively** | Cypher-native implementations were tested first; only reproduced gaps were ported. | Diff reconcile churn is regression-covered; Composer redraw/overscroll, stream anchoring, transcript copy/viewport, duplicate Codex auth tabs, and decorated ranges are implemented or covered by current Cypher tests. |
| **Evaluate later — P2** | A coherent protocol or major subsystem project, not an incremental reliability patch. | Pull-first HTTPS and reconnect architecture; native Claude/Codex; first-class Reasoning; large model picker; side-by-side diff; PR status; custom paths; native OpenCode/Cursor; future Linux UI work. |
| **Already equivalent / no work item** | Cypher already has the invariant or a deliberate stronger/different implementation. | Stable account ordering; MCP credential preservation; sibling-dial backoff reset; Comments/comment-only steers; atomic new-chat restoration; connection truth; Pi/Side Chat/`@Session`/Session Fork/Child Chats. |
| **Do not bring over** | Product-specific, architecturally incompatible, unsafe, or explicitly out of scope. | Upstream subagent document/right-tab model; ACP retirement; Zeron branding/releases/TestFlight; upstream GPUI fork; forced Codex full-access policy; bulk merge/cherry-pick. |

### P0 — port now: data integrity and crash recovery

These are the only items that should be treated as immediate reliability work.
They can silently lose history, hide chats, or strand user commands.

#### Chat2 Frontier, cursor contiguity, and bounded gap repair

Port the complete sequence:

```text
2c9e306 → 786e2d6 → e8ca1f3 → 7754391
```

and keep the corresponding iOS protocol port `08e53cd` in sync where the iOS
client is maintained.

The current Cypher implementation still contains the vulnerable shapes that
the upstream sequence fixed:

- `crates/engine/src/chat2_host.rs::contains_frontier` treats an empty
  Frontier as contained; an empty or encoded-empty Frontier cannot prove that
  local state contains server history.
- `crates/sync/src/chat_client.rs` advances receive progress with
  `max(sequence)`-style logic for rows and acknowledgements; a missing
  intermediate row can therefore be skipped.
- There is no complete bounded gap-repair and one-time cursor-amnesty
  protocol.

The Cypher implementation should:

1. Treat empty and encoded-empty Frontier values as **not contained**.
2. Advance the applied receive cursor only across contiguous rows.
3. Hold later rows when a sequence gap exists.
4. Request bounded repair from the last contiguous cursor.
5. Redial or fail visibly after the repair budget is exhausted.
6. Re-verify a cursor above the checkpoint once, rather than trusting it
   indefinitely.

Acceptance tests:

- empty Frontier and encoded-empty Frontier fetch the checkpoint;
- rows `1, 2, 4` leave the cursor at `2`, never at `4`;
- gap repair is bounded;
- a cursor above the checkpoint receives exactly one amnesty/revalidation;
- Rust, Edge, and iOS wire tests agree on the same behavior.

#### Registry cursor integrity and safe orphan deletion

Port:

```text
1fc6843 → 28eb39b
```

Current Cypher needs to distinguish server rows that have been received from
rows that have actually been applied to the local Registry projection.
`crates/doc/src/registry.rs` currently has paths that can move progress over
unapplied rows. In addition, `crates/engine/src/spaces.rs` orphan cleanup must
not decide that a chat is absent based only on an incomplete local Registry
view.

The port must provide:

- separate received/applied progress;
- gap holding instead of cursor jumps;
- a resynchronization epoch/latch;
- a server-synchronized or first-reconcile gate before `sweep_orphans`;
- unreadable HTTP acknowledgements treated as retryable, not successful.

Acceptance tests:

- an ACK cannot advance applied progress over an unapplied row;
- a missing row triggers resynchronization;
- orphan sweep deletes nothing before the initial Registry/server-truth gate;
- pending reseed or incomplete reconciliation also blocks deletion;
- local-only mode has an explicit first-reconcile equivalent.

#### Dead-command recovery and retry re-issue

Port:

```text
446ffbf → abacb45
```

Cypher intentionally uses mark-processed-before-execute in
`crates/engine/src/doc_host.rs`, backed by the command ledger in
`crates/doc/src/commands.rs`. That ordering is safe only if the crash window
between marking and execution is recoverable. Without recovery, a process
death can leave a command marked in the processed ledger while its outcome was
never written.

The port must:

- identify the mark-processed/execute crash gap on boot;
- terminalize genuinely dead commands instead of leaving invisible Pending
  ghosts;
- permit expired undelivered sends to be re-issued;
- give each retry a fresh attempt ID while retaining the stable logical
  message/command identity;
- preserve idempotence so retry does not duplicate transcript rows.

Acceptance tests:

- crash injection between mark and execute never leaves a permanently hidden
  Pending command;
- a dead Run is deterministically terminalized or re-executed idempotently;
- a retry has a new attempt ID and the original message identity;
- expired undelivered sends can be retried;
- Steer/Interrupt commands are superseded or re-issued according to their
  command semantics.

#### Forward-compatible workspace rows

Port:

```text
5306be2
```

Unknown future harness/config enum values must not make an entire chat row
undecodable. `crates/doc/src/workspace.rs` should preserve the row and degrade
only the unknown configuration portion. Do not add OpenCode-specific product
behavior merely because the upstream wire enum included it.

Acceptance test: a row containing an unknown future harness/config value
remains visible and readable, with a safe fallback configuration.

#### Upload staging sweep race

Port the safety part of:

```text
061e6ec
```

`crates/engine/src/uploads.rs` can currently treat a fresh empty staging
directory as immediately expired. A concurrent sweep can remove a directory
between its creation and the first uploaded chunk.

Add a directory mtime grace period for empty staging directories and test
concurrent append plus sweep. Preserve Cypher's profile-scoped upload roots,
path jail, and attachment privacy rules.

### P1 — port next: durable delivery

P1 begins only after the P0 command and cursor semantics are stable.

#### Durable worktree creation

Port:

```text
3e777c4 + c53ecd1
```

`WorktreeSpec` should travel with the durable Run command. Worktree creation
belongs to command draining on the host device, not to a pre-queue UI action.
A request for a new worktree must either produce the requested worktree or
remain visibly Queued/Failed; it must never silently run in the base checkout.
Relay calls need deadlines, and failed pre-queue operations must not leave
orphaned managed worktrees.

#### Queue-first attachments and real transfer progress

Port:

```text
8f3bce8 + 48ff777 + c3d2981
```

Persist the user's send intent before moving potentially slow attachment
bytes. During command drain, transfer attachments with larger chunks, a
bounded parallel window, and an overall deadline. Progress must reflect relay
transfer progress rather than only local staging.

Keep Cypher-specific behavior:

- attachments remain on the host device;
- reads remain proxied through the owning device;
- profile-scoped upload roots and path validation remain in force;
- pending/local and final attachment identities must not cause a post-send
  thumbnail blink.

#### Explicit delivery state, fallback, and retry

Port the delivery semantics of:

```text
9055e01 + d4d4045
```

against Cypher's command ledger rather than copying the upstream state
implementation. Cypher needs durable, user-visible distinctions between
Queued, Retrying, Delivered/Applied, Failed, and Superseded/Cancelled.
Disconnected delivery should be recoverable through the existing device/relay
topology. A retry must preserve logical message identity and original send time
while minting a fresh attempt identity.

#### New-chat atomic restoration

`da792da` is **not a separate implementation task** at present. Cypher already
has the basic atomic new-chat restoration behavior. Add end-to-end tests for
the complete worktree/upload/queue failure matrix; only port missing behavior
if those tests expose a real gap.

#### Indexed file search

Port the backend performance work from:

```text
447b689
```

Retain Cypher's session-first `@Session` picker and shared keyboard index.
Replace repeated full repository walks with a bounded cached index, path-aware
fuzzy ranking, and bounded ranking/index work. Acceptance requires equivalent
results and materially lower repeated-query latency on a large repository.

#### Session cycling

`2ca44ae` is a useful but lower-priority navigation port. Implement it against
Cypher's `NavHistory`, hidden Child Chats, Side Chat tabs, cross-space Sidebar,
and current tab semantics. Do not copy the upstream session list model.

### Verify, then port selectively

These commits should not be accepted as automatic “must merge” work. First add
a regression test, profile, or manual reproduction against Cypher's own
implementation:

| Commits | Question to answer |
| --- | --- |
| `74f4abe` | Does Cypher's watcher/debounce/repair loop still spawn `git diff` repeatedly during a quiet period? |
| `a00aa61` | Does the gpui Composer actually redraw while idle, and is it measurable? |
| `266262a` | Does wheel input at the Composer boundary escape into the Transcript? |
| `f8437ea` | Does Cypher's existing stick-to-bottom spring and Working trailer still move the viewport incorrectly? |
| `6f9f834` + `3a3fdca` | Is whole-entry copy still missing, given Cypher already has per-row copy? |
| `e744d55` | Is viewport restoration incorrect in Cypher's own virtualized transcript? |
| `f6911c3` | Are decorated ranges actually wrong across Cypher's soft wraps? |
| `bca16a3` | Can the current browser-poll/Reopen flow still open duplicate Codex auth tabs? |
| `89aa28d` / `46de808` | Does Cypher need side-by-side diff or no-newline hunk pairing after its own diff parser is exercised? |

These are good targeted fixes if the problem is reproduced, but blindly copying
them risks fixing a problem that Cypher does not have.

### P2 — evaluate as independent projects

The following are valuable candidates, but they change protocols or major
subsystems and must not be mixed into the P0/P1 reliability batches.

#### Pull-first HTTPS, authentication hardening, and reconnect architecture

Evaluate this as one desktop/Edge/iOS project:

```text
22b5a67
3bea7a5
3bb7d11
7e1eaea
0bd6a6b
ed8cd60
449a1db
c42f68b
```

The potential benefits are significant: HTTP bootstrap before WebSocket,
Bearer headers instead of query-string tokens, bounded responses,
event-driven reconnect, relay liveness, and visible pull failures. However,
isolated patches can create incompatible acknowledgement semantics. Design and
test the three participating clients and Edge behavior together.

The repository contains an iOS client even though older architecture text
called mobile out of scope. That text must not be used to justify a Rust-only
protocol change; if iOS remains maintained, it is part of this protocol
project.

#### Native harness projects

Evaluate separately:

```text
579678d   # Native Claude over stream-json
0ce7fc2   # Native Codex over app-server JSON-RPC
```

Cypher currently retains ACP paths and has its own Pi-native path. A native
driver may be useful, but it must pass compatibility, resume, steering,
interrupt, permission handling, and process-reaping tests and integrate with
durable Child Chats. Do not retire ACP or copy the Codex
`danger-full-access`/`approvalPolicy: never` behavior without an explicit
Cypher security decision.

Cursor SDK (`c7c7ce6`, `e21d146`, `bb90e24`, `69778b7`) and native OpenCode
HTTP/SSE (`bf63444` and related commits) are also separate harness projects,
not incremental ports.

#### Transcript, model, source-control, and path projects

Evaluate independently:

- `aa9f8bf` + `fed1d64`: first-class Reasoning, only after persistence and
  rendering semantics are defined for Transcript, tool groups, Comments, Side
  Chat, Child Chats, and Session Fork;
- `0434895`: virtualized model picker when actual catalogs justify the
  complexity, retaining provider attribution;
- `89aa28d`: side-by-side diff after diff correctness/resource behavior;
- `e2d6ea2` through `60965ad`: pull-request status, which spans proto, device
  RPC, `gh`, caching, Sidebar, Composer, and iOS;
- `1d30023` + `c6ad564`: multi-drive/custom paths only with canonical path
  validation and a Cypher project-picker design;
- Linux CSD/backdrop blur and other future platform UI changes only for the
  supported Cypher GUI, not as a GPUI fork replacement.

### Already equivalent or deliberately different

The following should not remain on the implementation backlog unless a new
regression is found:

| Upstream item | Current Cypher decision/evidence |
| --- | --- |
| `7207f74` | Account rows already use stable creation/identity ordering. |
| `f5fb9b9` | Claude account activation updates only the intended account fields and preserves other machine-level MCP/plugin configuration. |
| `989be0a` | Wake/reconnect handling already resets sibling-dial backoff after a successful wake. |
| `2b4d333` + `56ac020` | Cypher Comments already support the relevant Transcript/Git Diff/Terminal entry points, comment-only steers, and visible/effective prompt separation. |
| `dc6b8e5` | Cypher does not expose the upstream quota UI; token-usage display is a documented parity exclusion. |
| `da792da` | Basic atomic new-chat restoration exists; add failure-matrix tests rather than porting the upstream implementation. |
| `b98d698` | Cypher already has part of the durable connection/queued truth model; complete it through P1 delivery state work rather than copying upstream UI. |
| Pi RPC, Child Chats, Side Chat, `@Session`, Session Fork | These are Cypher-native architecture, not upstream features to replace. |

### Explicitly do not bring over

1. Do not merge `upstream/main` or bulk cherry-pick the audited range.
2. Do not import the upstream per-subagent document/right-pane-tab model
   (`06da4c4` and its visualization follow-ups). Cypher's durable Child Chat
   model, ownership, Sidebar behavior, synchronization, Side Chat, and Session
   Fork supersede it. Only individual wire-level event-normalization lessons
   may be reimplemented in Cypher's model.
3. Do not replace Pi RPC, Side Chat, `@Session`, Session Fork, or Child Chats
   with upstream equivalents.
4. Do not retire Claude/Codex/Cursor ACP machinery from `60887f7` until native
   replacements pass compatibility and migration gates.
5. Do not import Zeron branding, landing content, release markers, release
   bytes, Bundle IDs, App Store/TestFlight workflows, WorkOS configuration or
   credentials, or `FUNDING.yml`.
6. Do not adopt the upstream GPUI fork wholesale (`ce35a1a`). Inspect
   Cypher's pinned Zed revision and port only a specifically reproduced fix.
7. Do not copy Codex's forced `danger-full-access` and
   `approvalPolicy: never` security policy.
8. Do not copy upstream UI styling literally. Cypher's floating-card,
   monochrome, Side Chat, and four-pixel-gutter language is intentional; take
   behavior, not visual architecture.

### Recommended execution order

```text
Batch 1 — P0 data safety
  2c9e306 → 786e2d6 → e8ca1f3 → 7754391
  1fc6843 → 28eb39b
  446ffbf → abacb45
  5306be2
  061e6ec

Batch 2 — P1 durable delivery
  command attempt/dead recovery
  3e777c4 + c53ecd1
  8f3bce8 + 48ff777 + c3d2981
  9055e01 + d4d4045
  447b689
  2ca44ae

Batch 3 — verify-then-fix UX/resource items
  74f4abe, a00aa61, 266262a, f8437ea,
  6f9f834, 3a3fdca, e744d55, f6911c3, bca16a3

Batch 4 — independent architecture projects
  pull-first HTTPS/reconnect
  Native Claude / Native Codex / other harnesses
  Reasoning
  model picker, diff, PR status, custom paths, future Linux UI
```

Every port must be a Cypher change with Cypher tests. The upstream commit ID
is a source/reference identifier, not a merge instruction.

### Verification and rollout rules

P0 should be landed as small, independently reviewable changes. Do not bundle
protocol cursor changes, command-ledger changes, and UI work into one large
port. For each change:

1. Add the invariant-level unit tests first.
2. Add failure injection for the relevant crash, dropped row, timeout, or
   concurrent filesystem operation.
3. Run the Rust sync/doc/engine tests and the matching Edge tests.
4. Run the iOS XCTest protocol vectors whenever the shared Chat2 or Registry
   semantics change.
5. Extend the two-device end-to-end smoke test with at least one negative
   case before calling the item complete.

The minimum P0 verification set is:

```text
cargo test -p cypher-sync -p cypher-doc -p cypher-engine
Edge chat2/registry test suite
apps/ios XCTest protocol vectors (when the iOS target is available)
two-device e2e smoke with an injected row gap
two-device e2e with a crash between command mark and execute
```

The Edge chat2 frame layout must remain cross-language compatible. If the
change requires a new acknowledgement or cursor field, update Rust, TypeScript
Edge, and Swift vectors together. A Rust-only fix is not complete when another
maintained client can still advance the old cursor semantics.

P1 depends on the P0 command-ledger changes. In particular, WorktreeSpec,
queue-first attachments, and Failed/Retry delivery should not be implemented
as disconnected UI states; they must be durable command/outbox states that
survive restart and relay loss.

### Risks and assumptions to resolve during implementation

- Changing empty Frontier handling from “contained” to “not contained” can
  cause an extra checkpoint fetch for fresh rooms. This is intentional and is
  the safe direction.
- Separating Registry received and applied progress may require a persisted
  state version/migration. Preserve backward-compatible reads.
- Queue-first attachment delivery changes optimistic-echo timing. Reconcile
  pending attachment identities with final/cache identities without exposing
  a thumbnail blink or a duplicate message.
- `da792da` and `b98d698` contain useful failure-truth ideas, but Cypher should
  complete them through its own command and UI state model, not duplicate
  upstream UI.
- Confirm the exact Edge behavior for a non-empty checkpoint with an empty or
  encoded-empty Frontier; regardless of the answer, the client must take the
  safe fetch path for an empty claim.
- Confirm Registry server ACK ordering before choosing the precise
  received/applied representation; do not infer applied state merely from an
  ACK sequence number.
- Confirm whether the maintained iOS target can build with its current Xcode
  toolchain. If not, keep the shared protocol vectors and mark the iOS binary
  validation as a release gate rather than silently omitting the port.

## Original grouped inventory

The sections below retain the detailed historical audit and chronological
ledger. Their dispositions are subordinate to the **Current Cypher product
decision** above, which incorporates the current code review and distinguishes
confirmed gaps from features that are already equivalent or require a separate
architecture decision.

These changes are worth adopting, but they should be **manually ported** into
Cypher rather than merged or bulk cherry-picked. The order reflects data-loss
risk and operational value, not upstream release order.

### P0 — port first: data integrity and crash recovery

| Commits | Capability | Why Cypher needs it |
| --- | --- | --- |
| `2c9e306` → `786e2d6` → `e8ca1f3` → `7754391` | Chat2 Frontier validation, cursor amnesty, contiguous receive cursor, bounded gap repair | Prevents empty Frontiers and sequence gaps from silently skipping history or randomly hanging new sessions. |
| `1fc6843` → `28eb39b` | Registry cursor integrity, resync latch, server-synchronized orphan sweep | Prevents cursor jumps over unapplied rows and deletion based only on incomplete local Registry state. |
| `446ffbf` → `abacb45` | Dead-command recovery and retry re-issue | Closes the mark-processed-before-execute crash gap and makes expired undelivered commands retryable. |
| `5306be2` | Lenient future-config decoding | Keeps chats readable when a newer peer writes unknown harness/config enum values. |
| `061e6ec` | Upload staging sweep mtime guard | Prevents a fresh empty staging directory from being deleted while an upload is starting. |
| `74f4abe` | Diff reconcile deduplication | **Verify first** against Cypher's watcher/debounce/repair implementation; port only if the repeated-spawn regression is reproduced. |

### P1 — port next: durable delivery

| Commits | Capability | Why Cypher needs it |
| --- | --- | --- |
| `3e777c4` + `c53ecd1` | Durable worktree specification and truthful queued state | Worktree creation belongs to the durable Run command and must never silently degrade to the base checkout. |
| `8f3bce8` + `48ff777` + `c3d2981` | Queue-first attachments, bounded parallel transfer, real progress | Records user intent before moving bytes, survives slow links, and reports actual relay progress. |
| `9055e01` + `d4d4045` | Relay fallback, explicit Failed state, `RetryDelivery` | Makes delivery outcomes durable and recoverable instead of optimistic or ambiguous. |
| `da792da` | Atomic new-chat send restoration | **Already substantially equivalent** in Cypher; add the complete failure-matrix tests rather than porting the upstream implementation. |
| `989be0a` | New-chat join backoff reset | **Already equivalent** in Cypher's wake/reconnect handling; do not duplicate it. |

### P1 — small, high-value correctness ports

| Commits | Capability | Why Cypher needs it |
| --- | --- | --- |
| `a00aa61` | Stop Composer idle redraw | **Verify first** with gpui profiling; Cypher's Composer is independently implemented. |
| `266262a` | Contain Composer overscroll | **Verify first** with a boundary-wheel regression test. |
| `7207f74` | Stable account ordering | **Already equivalent**; account ordering is stable in Cypher. |
| `bca16a3` | Single Codex authentication tab | **Verify first** against Cypher's browser-poll/Reopen flow. |
| `dc6b8e5` | Correct Codex monthly quota window | **Not applicable**; Cypher does not expose this quota UI and token usage is a parity exclusion. |
| `f5fb9b9` | Preserve live Claude MCP OAuth/plugin secrets | **Already equivalent**; Cypher preserves unrelated machine-level MCP/plugin configuration. |
| `447b689` | Cached indexed file search | **Port backend performance work** while retaining Cypher's `@Session` behavior. |
| `f8437ea` | Stable live-stream anchoring | **Verify first** against Cypher's existing stick-to-bottom/spring implementation. |
| `6f9f834` + `3a3fdca` | Entry-level message copy fidelity | **Port selectively** only if whole-entry copy remains missing; Cypher already has per-row copy. |
| `2ca44ae` | Session cycling shortcut | **Port later** against Cypher's `NavHistory`, hidden children, Side Chat tabs, and cross-space sessions. |

### P2 — evaluate as coherent architecture projects

These are valuable, but each changes a protocol or major subsystem and should
not be mixed into the P0/P1 reliability batches.

| Commits | Project | Cypher-specific condition |
| --- | --- | --- |
| `22b5a67`, `3bea7a5`, `3bb7d11`, `7e1eaea`, `0bd6a6b`, `ed8cd60`, `449a1db`, `c42f68b` | Pull-first HTTPS, Bearer-token hardening, bounded responses, event-driven reconnect, relay liveness | Port desktop, Edge, and iOS behavior together; isolated transport patches can create incompatible acknowledgement semantics. |
| `579678d` | Native Claude over stream-json | Keep Pi as the flagship; integrate Claude events into Cypher Child Chats and validate the partially undocumented stdio permission channel. |
| `0ce7fc2` | Native Codex over app-server JSON-RPC | Version-test the experimental API and map Cypher sandbox/approval settings instead of copying forced full access. |
| `aa9f8bf` + `fed1d64` | First-class Reasoning | Land only after transcript persistence/rendering semantics are designed for Cypher Comments, Side Chat, and Session Fork. |
| `0434895` | Virtualized thousand-model picker | Worth adopting when real catalogs justify it; retain provider attribution and Cypher's current picker behavior. |
| `89aa28d` | Side-by-side diff | Useful product feature after diff correctness and resource-loop fixes are ported. |
| `e2d6ea2` through `60965ad` | Pull-request status across desktop, devices, and iOS | Valuable but broad: proto, device RPC, `gh`, caching, Sidebar, Composer, and iOS must land together. |
| `1d30023` + `c6ad564` | Multi-drive/custom-path projects | Require canonical path validation and a Cypher-specific project-picker design. |

### Do not bring over as architecture

- Do not merge `upstream/main` or bulk cherry-pick the audited range.
- Do not adopt upstream's per-subagent document/right-pane-tab model; Cypher's
  durable Child Chats supersede it.
- Do not replace Cypher's native Pi RPC, Side Chat, `@Session`, or Session Fork
  architecture with upstream equivalents.
- Do not import Zeron branding, landing content, release tags, bundle IDs,
  TestFlight credentials, WorkOS configuration, or release bytes.
- Do not remove Claude/Codex ACP paths until native replacements have passed
  compatibility, resume, steering, interrupt, and process-reaping gates.
- Do not copy Codex's forced `danger-full-access` and
  `approvalPolicy: never` policy without an explicit Cypher security decision.

## Scope

```text
Former upstream: https://github.com/zeronsh/comet
Merge base:      9ab250ceb6317d080a8429435cb15a9eaef5663e
Audit point:     20483e6fdf200cfd025bf6680b5a08089fbe45fa
Upstream date:   2026-08-23T20:59:03+03:00
Range:           9ab250c..upstream/main
```

The upstream side contains:

```text
265 commits
186 non-merge commits
79 merge commits
199 changed files
40,998 insertions
4,982 deletions
v0.2.3 through v0.2.27
```

For comparison, Cypher's side of the same divergence contains:

```text
18 commits
18 non-merge commits
0 merge commits
283 changed files
34,528 insertions
6,120 deletions
```

The feature inventory below groups related commits into coherent capabilities.
The chronological ledger at the end records every upstream non-merge commit in
the audited range, including releases, landing-page changes, formatting, and
test-only changes.

## Adoption labels

- **Port** — useful to Cypher and should be manually reimplemented against
  Cypher's current architecture.
- **Equivalent** — Cypher already has the same invariant or a stronger
  implementation.
- **Evaluate** — useful, but dependent on a larger design or prerequisite.
- **Defer** — not currently important enough to justify its cost.
- **Do not port** — tied to incompatible Zeron architecture, branding, release
  infrastructure, or product decisions.

## 1. Sync, CRDT, and registry integrity

### Pull-first HTTPS synchronization

Commits:

```text
42a7303  Cap mobile warm dials, overlap checkpoint/backfill, flush sends
7129b50  Make iOS checkpoint downloads survive redials
22b5a67  Add pull-first HTTPS transport; demote WebSocket to enhancement
3bea7a5  Wait for known server state before healing/retiring pushes
6cf87a3  Drain the iOS socket frame buffer during checkpoint fetch
3bb7d11  Surface pull-path failures on desktop and iOS
0bd6a6b  Use Bearer headers, body caps, and bounded row responses
7e1eaea  Claim Chat2 rooms on first HTTP contact
ed8cd60  Add event-driven reconnects, network path monitoring, and tighter caps
449a1db  Track peer-relay liveness and park dials when the registry is dark
c42f68b  Port reconnect and relay-liveness behavior to iOS
```

Features and invariants:

- One-round-trip HTTP bootstrap rather than requiring a WebSocket connection
  before useful synchronization can begin.
- WebSocket becomes an acceleration/wake channel rather than the only viable
  data path.
- Checkpoint download, row backfill, and queued sends overlap.
- Reconnect is event-driven by online/path/liveness signals rather than broad
  polling.
- Mobile warm dials and response sizes are bounded.
- Authentication tokens move from query strings to `Authorization: Bearer`.
- Server acknowledgement is authoritative; arbitrary intermediary `4xx`
  responses do not retire durable client writes.
- Pull failures become visible instead of silently stranding synchronization.

Cypher disposition: **Evaluate as one coherent security/reliability project**.
Do not port isolated pieces without the matching Edge, desktop, and iOS
protocol changes.

### Frontier, cursor contiguity, and gap repair

Commits:

```text
2c9e306  Empty checkpoint Frontier is not considered contained
786e2d6  Encoded-empty Frontier is a vacuous claim
e8ca1f3  Re-verify a cursor above the checkpoint once
7754391  Enforce Chat2 cursor contiguity and bounded gap repair
08e53cd  Port cursor contiguity, gap repair, and retry re-issue to iOS
```

Features and invariants:

- A zero or encoded-empty Frontier cannot prove that local state contains
  server history.
- A suspicious cursor above a checkpoint receives one bounded amnesty pass.
- Receive cursors advance only across contiguous rows.
- Missing sequences are repaired explicitly instead of skipped with
  `max(sequence)`.
- Retry re-issues do not leave new sessions randomly hanging.

Cypher disposition: **Port, highest priority**. Current Cypher contains the
same vulnerable empty-Frontier and `max(seq)` patterns.

### Registry cursor integrity and safe orphan deletion

Commits:

```text
1fc6843  Prevent Registry cursor jumps over unapplied rows
28eb39b  Wait for server truth before orphan sweep; retry unreadable HTTP ack
```

Features and invariants:

- Separate received/acknowledged progress from applied progress.
- Hold rows across sequence gaps rather than advancing the cursor.
- Introduce a resynchronization epoch/latch.
- Delete apparent orphan chats only after the Registry has synchronized with
  server truth.
- Treat an unreadable HTTP acknowledgement as retryable, not success.

Cypher disposition: **Port, highest priority**. Current
`sweep_orphans` lacks the server-synchronized gate.

### Forward-compatible workspace rows

Commit:

```text
5306be2  Keep chat rows when newer peers send unknown harness config enums
```

Unknown future harness/config values no longer make an entire durable chat row
undecodable. iOS also learns the OpenCode harness identifier.

Cypher disposition: **Port the lenient decoding invariant**; do not adopt
OpenCode-specific product behavior merely because the wire enum exists.

## 2. Durable send, worktree, and attachment delivery

### Durable worktree creation

Commits:

```text
3e777c4  Put worktree creation on the durable command plane; add relay deadlines
c53ecd1  Never silently degrade New-worktree; display undelivered sends as Queued
f4383e3  Resolve default branches with gh instead of Git transport
```

Features and invariants:

- `WorktreeSpec` travels with the durable Run command.
- Worktree creation happens when the host drains the command rather than
  before `QueueCommand`.
- Relay calls have deadlines.
- A request for a new worktree either produces that worktree or reports a
  truthful queued/failure state; it never silently runs in the base checkout.
- Default-branch lookup avoids Git-network operations where GitHub metadata is
  available.

Cypher disposition: **Port after the data-integrity batch**.

### Queue-first attachments and transfer progress

Commits:

```text
48ff777  Larger chunks, parallel upload window, total deadline, live progress
a729ed4  Render progress on the sending thumbnail
8f3bce8  QueueCommand first; transfer attachment bytes while draining
e8f9e03  Restore sending indicator and eliminate thumbnail blink/clipping
c3d2981  Drive thumbnail percentage from real relay transfer progress
061e6ec  Protect fresh empty staging directories during upload sweep
6da274d  Port queued attachment/worktree behavior to iOS
```

Features and invariants:

- Durable user intent is queued before potentially slow attachment transfer.
- Uploads use larger chunks and a bounded parallel window.
- Overall upload duration is capped.
- Progress reflects actual relay transfer rather than only local staging.
- Pending/local and final/cache attachment identities are reconciled without a
  post-send image blink.
- Fresh empty staging directories are protected by mtime during sweeping.

Cypher disposition: **Port**, preserving Cypher's attachment roots and path
validation. The thumbnail changes depend on queue-first attachment identity
and should land afterward.

### Explicit delivery state, retries, and atomic restoration

Commits:

```text
9055e01  Add peer-relay QueueCommand fallback and flaky-network test suite
d4d4045  Add explicit Failed state, RetryDelivery, and send-time timestamps
b98d698  Show truthful connection, composer, and Queued status
061e6ec  Add degrade hysteresis and calmer connection UI
3bfe306  Simplify connection status to a bare spinner
9116ff6  Make the connection spinner grayscale
da792da  Stage new-chat sends atomically and restore canvas state on failure
989be0a  Reset new-chat join backoff after a sibling dial succeeds
```

Features and invariants:

- A failed direct delivery can fall back through peer relay.
- Delivery distinguishes queued, failed, retrying, and delivered states.
- Retrying does not forge the original send time.
- New-chat prompt, attachments, and configuration are staged atomically and
  restored together if publication fails.
- Connectivity UI reports durable truth rather than optimistic success.

Cypher disposition: **Port the delivery state machine**, then adapt the UI to
Cypher's floating-card language rather than copying Zeron visuals.

### Dead-command and retry recovery

Commits:

```text
446ffbf  Mint a fresh attempt ID; terminalize dead commands
abacb45  Allow expired undelivered sends to be re-issued
```

Features and invariants:

- A retry has a fresh attempt identity while retaining stable logical message
  identity.
- Commands that were marked processed but never resolved do not remain
  invisible ghosts forever.
- Expired, undelivered sends can be retried.

Cypher disposition: **Port, highest priority**. Adapt it to Cypher's command
journal so a crash around mark-processed/execute cannot duplicate transcript
rows.

## 3. Native agent drivers

### Native Claude Code driver

Commit:

```text
579678d  Drive Claude directly over stream-json
```

Features:

- Spawn the installed `claude` CLI directly.
- Use `--input-format stream-json` and `--output-format stream-json`.
- Map Claude stream frames to the common harness event model.
- Resume with Claude's native session ID.
- Send steers as additional user frames.
- Route permission and `AskUserQuestion` requests over the stdio control
  channel.
- Preserve native `result` completion and background-agent wake turns.
- Expose subagent activity through `parent_tool_use_id`.
- Escalate interrupt from protocol request to `SIGTERM`/`SIGKILL`.

Cypher disposition: **Evaluate as a dedicated Native Claude project**. It can
replace `claude-agent-acp`, but depends on a partially undocumented Claude
control channel and must integrate with Cypher Child Chats rather than
upstream's subagent documents.

### Native Codex driver

Commit:

```text
0ce7fc2  Drive Codex directly through app-server JSON-RPC
```

Features:

- Spawn `codex app-server`.
- Perform the JSON-RPC initialize/initialized handshake.
- Start or resume native Codex threads.
- Start, steer, and interrupt turns through app-server methods.
- Normalize agent messages, reasoning, tool lifecycle, usage, and terminal
  turn states.
- Route Codex child threads as subagent events.
- Preserve a rejected steer as the next turn instead of dropping it.

Cypher disposition: **Evaluate as a dedicated Native Codex project**. The
app-server API is experimental and the upstream implementation was validated
against a specific CLI release. Do not copy its forced
`danger-full-access`/`approvalPolicy: never` behavior without a Cypher security
decision.

### Native Cursor driver and account integration

Commits:

```text
c7c7ce6  Add a pinned @cursor/sdk shim driver
e21d146  Add in-app Cursor browser login and Accounts integration
bb90e24  Discover Cursor models live and apply typed model options
69778b7  Isolate each SDK run's agent store to avoid concurrent-run locking
```

Cypher disposition: **Defer**. This is a separate SDK/versioning/account-store
project and not a safe incremental patch to the existing ACP path.

### OpenCode integrations

Commits:

```text
0213cac  Add an OpenCode ACP harness with sidecar subagent events
479a341  Repair the OpenCode probe example
7c6c123  Surface provider failures instead of remaining Working forever
296813f  Populate effort choices from model variants
fde9b4b  Harden OpenCode model discovery
bf63444  Replace OpenCode ACP with native HTTP/SSE
0b421cc  Bound every OpenCode HTTP call
a401432  Require a live event subscription before the first prompt
4fd3557  Show models only from connected providers
```

Native OpenCode features include:

- Direct HTTP/SSE transport.
- Bounded discovery, startup, and turn calls.
- Subscription readiness before sending the first prompt.
- Provider-aware models and reasoning variants.
- Explicit provider failure and terminal-state handling.

Cypher disposition: **Defer as a coherent new harness project**.

### Remaining ACP and native-driver compatibility

Commits:

```text
60887f7  Retire Claude/Codex/Cursor adapter machinery
2133bae  Harden Grok ACP
eda27e8  Support Grok model switching
c06ed22  Let native drivers rediscover slash commands
76b49f0  Default-enable agents only when their CLI is installed
8528e3b  Permit installed harnesses to be disabled; require installed CLI
a8ef0aa  Remove unrunnable composer fallbacks
aacb621  Handle empty agent catalogs
```

Cypher disposition:

- Installed-agent gating and empty-catalog safety are **Equivalent or small
  ports**.
- Native command discovery is **Evaluate** alongside each native driver.
- The broad removal of adapter machinery is **Do not port** until replacement
  native drivers have shipped and old sessions have a migration policy.

## 4. Subagent visualization and steering

### Upstream subagent document/tab model

Commits:

```text
06da4c4  Per-subagent documents, spawn chips, right-pane tabs
bed83ba  Fix three live subagent visualization findings
e713383  Add contextual titles and a streaming runway
7fd861e  Add terminal statuses and designed spawn chips
0c31ec4  Label Agent chips and improve Codex paragraphing
06fe8d1  Add scroll-follow, jump pill, and top fade
1eb0bef  Open subagent tabs at the end
9a12547  Add bot iconography
6d00440  Link spawn chips to subagent tabs
80a3f30  Show a Working trailer and parent steer messages
54b2d45  Open with the prompt; forward OpenCode/Grok user messages
7a05159  Keep spawn chips visible outside collapsed tool groups
6530b88  Center chip header content
5019dc1  Render only agent calls with documents as subagent chips
18987da  Do not bind background shell settlements as subagents
569793e  Gate subagent binding by call genus for every harness
cb2f30d  Show the subagent model on spawn chips
```

Cypher disposition: **Do not port the document/right-tab architecture**.
Cypher's durable parent/child Chats, exactly-once `StartSubagent`, Sidebar
hiding, navigation history, RPC ownership, and synchronization supersede it.
Individual protocol-normalization findings and visual details may still inform
Cypher-native Child Chat UI.

### Subagent steering and harness-specific event repair

Commits:

```text
a46b701  Add Grok subagent lifecycle events and transcript tailing
1f94405  Treat Codex child userMessage items as steers
454a1dd  Make subagent steers durable across harnesses
181d667  Do not interpret Claude interruption markers as steers
```

Cypher disposition: **Port only the wire-level invariants if/when the
corresponding native harness lands**. Durable routing must terminate in
Cypher's Child Chat system, not upstream's per-subagent documents.

## 5. Reasoning, models, commands, and accounts

### First-class reasoning

Commits:

```text
aa9f8bf  Make Reasoning a first-class transcript part
fed1d64  Place Thinking inside the tool-group accordion
```

Features:

- Reasoning is represented independently from ordinary assistant text.
- Streaming, persistence, and transcript rendering preserve that distinction.
- Tool-heavy turns group their associated thinking coherently.

Cypher disposition: **Evaluate after synchronization/delivery hardening**.

### Large model catalogs

Commit:

```text
0434895  Virtualize model picker for thousand-model catalogs and show provider
```

Features:

- Model rows are virtualized/bounded for very large catalogs.
- Provider attribution disambiguates duplicate model labels.
- Selection remains responsive with thousands of entries.

Cypher disposition: **Port later**, especially if Pi/OpenCode catalogs grow.

### Accounts quality and credential preservation

Commits:

```text
7207f74  Keep account ordering stable
dc6b8e5  Display Codex free-tier quota as a monthly window
bca16a3  Prevent duplicate Codex authentication browser tabs
f5fb9b9  Preserve live machine-level MCP OAuth while switching Claude accounts
```

Cypher disposition:

- Stable ordering, Codex quota labeling, and duplicate-browser suppression are
  **small ports**.
- MCP OAuth/plugin-secret preservation is a **high-value credential-integrity
  port** if Cypher continues multi-account Claude switching. Use an explicit
  allowlist; never copy arbitrary secret-bearing state.

## 6. Composer, mentions, comments, and text editing

### Diff comments and folded prompt context

Commits:

```text
2b4d333  Comment on diff lines and stage comments for the next prompt
45d5562  Fold sent prompt context into a transcript pill
70a7a49  Refine review-side marker and card styling
56ac020  Support comment-only steers and preserve drafts
```

Cypher disposition: **Equivalent/stronger**. Cypher supports current-chat
Comments from Transcript, Git Diff, and Terminal; one-shot `agentPrompt`;
comment-only turns; restoration; and separate visible/effective prompts.

### Mention picker and indexed file search

Commit:

```text
447b689  Add full-width mention panel and cached indexed repository search
```

Features:

- Replace repeated repository walks with a cached, parallel file index.
- Bound index size and ranking work.
- Use path-aware fuzzy ranking.
- Show filename and directory in a full-width result row.

Cypher disposition: **Port the backend indexing/performance work** while
retaining Cypher's mixed `@Session`/file picker, shared keyboard index, and
session-first ordering.

### Composer sizing and interaction fixes

Commits:

```text
c4e8cd4  Stop composer reflow during panel resize
5d20db8  Adapt traits control to available width
79d05d5  Preserve a minimum chat-panel width
2bd9ee3  Format the resize implementation
a00aa61  Stop the composer idle redraw loop
266262a  Contain wheel overscroll inside the composer
89cfeef  Refine panel resize/alignment behavior
1cf7036  Select the entire text-field value on double click
```

Cypher disposition:

- Idle redraw and overscroll are **small performance/interaction ports**.
- Panel sizing must be reconciled with Cypher's floating cards, Side Chat, and
  four-pixel gutter.
- Double-click select-all is **optional product behavior**; it is not normal
  word-selection semantics and should be adopted intentionally.

## 7. Transcript and message presentation

Commits:

```text
f8437ea  Anchor live streams at the transcript end
6f9f834  Add entry-level message copying and spacing refinements
3a3fdca  Improve transcript copy fidelity and spacing parity
e744d55  Restore transcript viewport correctly
048f8a2  Repair the viewport test after merge
7a05159  Keep spawn chips visible outside tool-group collapse
fed1d64  Place thinking in the tool-group accordion
f6911c3  Fix decorated text ranges at soft wraps
```

Features:

- Pin streaming content only when the viewport is truly at the end.
- Avoid movement caused by a Working trailer or transient layout changes.
- Copy complete message entries with faithful text.
- Restore viewport location after navigation/layout.
- Keep decorations correct across soft-wrapped lines.

Cypher disposition: **Port selectively**. Transcript changes collide with
Comments, Side Chat selection, `@Session` chips, Session Fork controls, and
Cypher's own streaming behavior, so they require manual integration.

## 8. Diff, review, and source-control UX

### Resizable and side-by-side diff

Commits:

```text
b1214b6  Make the Changes pane freely resizable
89aa28d  Add side-by-side diff reading
46de808  Pair edits across no-newline markers
74f4abe  Stop diff reconcile churn from spawning endless git diff processes
```

Features:

- User-resizable diff panel.
- Split/side-by-side review mode.
- Correct hunk pairing around no-newline markers.
- Reconcile deduplication to prevent an idle subprocess/resource loop.

Cypher disposition: **Port correctness/resource fixes first**; evaluate the
split view against current Changes UI.

### Pull request status

Commits:

```text
e2d6ea2  Add checkout change-request status to proto
7fb1158  Resolve GitHub pull requests through gh
3adc155  Stream cached checkout change-request status
bdc8363  Track pull requests for local and remote sessions
72ab1d9  Add pull-request badges to Sidebar and Composer
2b157a0  Add streaming device RPC on iOS
ae70566  Show host pull requests in iOS session surfaces
ede19ac  Test pull-request status across device boundaries
0cb68c7  Frost the pull-request tooltip
46ddcf9  Find gh through login-shell PATH
4f46337  Preserve links when a peer cannot stream the method
2998ca4  Retry watching after a host upgrade
819fc4c  Use origin for untracked branches
048798d  Resolve uncached remote default branches
60965ad  Evict inactive change-request cache entries
0e7c6e5  Isolate the gh PATH test fixture
```

Cypher disposition: **Defer**. Useful product functionality, but it adds new
proto, device RPC, host cache, GitHub CLI, Sidebar, Composer, and iOS surfaces.
If adopted, keep the cache eviction and old-host compatibility behavior.

## 9. Projects, spaces, navigation, and shell UI

Commits:

```text
2ca44ae  Cycle sessions with Ctrl+Tab
1d30023  Reach additional drives from the project picker
c6ad564  Enumerate custom mounts and accept typed paths
35791f2  Label Alt as Opt on macOS
7ac14e6  Round project-picker section corners
0c84d0d  Restore full right-pane takeover
1fdf149  Restyle jump pills
0079972  Gate Git Add menu by workspace
2761213  Unify the new-session titlebar action
be01c65  Size the project menu with the Sidebar
a2db751  Correct the right-pane takeover icon
```

Cypher disposition:

- Session cycling is a **small useful port** if it respects Shell
  `NavHistory`, hidden child chats, Side Chat tabs, and all-project Sidebar.
- Multi-drive/custom-path project selection is **Evaluate** with canonical
  path validation.
- Most styling is **Do not copy literally**; apply only behavior compatible
  with Cypher's floating-card language.

## 10. Terminal, selection, and general desktop UI

Commits:

```text
f406c5b  Draw Linux caption controls with client-side decorations
4e0a89d  Add wgpu backdrop blur for frosted Linux floats
889b78e  Repair terminal rendering after reopening the Sidebar
3536a37  Fix selection-edge autoscroll and terminal scrollbar
f6911c3  Fix decorated ranges at soft wraps
ce35a1a  Repoint the GPUI fork and frost the change-request card
```

Cypher disposition:

- Terminal and selection correctness fixes are **manual ports**.
- Linux caption controls and backdrop blur are **Evaluate** for the future
  Linux GUI, not the current headless Linux release.
- Do not adopt the upstream GPUI fork wholesale; inspect whether Cypher's Zed
  revision already contains required fixes.

## 11. iOS reliability and distribution

### Runtime and synchronization

Commits:

```text
5bcf1f9  Single-flight refresh, stagger room dials, improve frame settling
390d6eb  Reveal cached transcripts after layout quiet
42a7303  Bound mobile warm dials and overlap synchronization work
7129b50  Survive checkpoint redials and gate room dials on Registry
6cf87a3  Drain checkpoint socket frames
3bb7d11  Surface pull failures
08e53cd  Port cursor/gap repair and retry re-issue
c42f68b  Port reconnect and relay liveness
6da274d  Port send truth, queued attachments, and worktree-on-drain
ac34d9f  Clean up iOS build warnings
```

Cypher disposition: selectively useful, but the current iOS project requires
Xcode 26-compatible tooling before full validation. Cypher already has
single-flight refresh and conservative sign-out semantics.

### App Store/TestFlight

Commits:

```text
b2b0b19  Configure the Zeron App Store bundle identifier
92b5732  Add permanent internal TestFlight workflow
8f62bcf  Add external TestFlight audience input
```

Cypher disposition: **Do not port literally**. Bundle IDs, App Store Connect
credentials, signing, audiences, and workflows are product-specific. The
general workflow design can inform a future Cypher TestFlight pipeline.

## 12. Release, landing, and repository administration

The upstream range publishes Zeron releases `v0.2.3` through `v0.2.27`, updates
the Zeron landing page after releases, adds sponsor metadata, and refreshes
landing testimonials.

Representative commits:

```text
18b915e..fb05600  Zeron v0.2.4 through v0.2.27 release markers
b9e90ce            Landing download update for the already-published v0.2.3
42d7161            Landing download update for v0.2.4
bbe7bee            Landing download update for v0.2.5
17925d3            Landing download update for v0.2.6
76ddfb6            Landing download update for v0.2.7
71b221f            Landing download update for v0.2.8
a6db86d            Landing download update for v0.2.9
fc111c1            Landing download update for v0.2.10
4daf42e            Add testimonials to the Zeron landing page
a6f05de            Add GitHub FUNDING.yml
```

Cypher disposition: **Do not port release bytes, tags, landing content,
branding, bundle IDs, or credentials**. Cypher uses immutable namespaced
`cypher-v*` releases and its own GitHub/R2/Cloudflare/WorkOS infrastructure.

## Recommended Cypher ordering

### Batch 1 — data safety

```text
2c9e306 → 786e2d6 → e8ca1f3 → 7754391
1fc6843 → 28eb39b
446ffbf → abacb45
5306be2
061e6ec (upload sweep subfix)
```

### Batch 2 — durable delivery

```text
command attempt/dead recovery (P0 prerequisite)
3e777c4 + c53ecd1
8f3bce8 + 48ff777 + c3d2981
9055e01 + d4d4045
447b689
2ca44ae
```

### Batch 3 — security and reconnect architecture

Evaluate as one unit:

```text
22b5a67
3bea7a5
3bb7d11
7e1eaea
0bd6a6b
ed8cd60
c42f68b
449a1db
```

### Batch 4 — targeted UX and resource fixes

```text
verify first:
74f4abe  Diff reconcile subprocess churn
a00aa61  Composer idle redraw loop
266262a  Composer overscroll containment
bca16a3  Duplicate Codex browser tab
f8437ea  Streaming transcript anchor
6f9f834 + 3a3fdca  Entry-level message copy
e744d55  Transcript viewport restoration
f6911c3  Decorated ranges at soft wraps
```

### Dedicated architecture projects

These should not be mixed into reliability batches:

```text
579678d  Native Claude
0ce7fc2  Native Codex
c7c7ce6  Native Cursor SDK
bf63444  Native OpenCode HTTP/SSE
aa9f8bf  First-class Reasoning
0434895  Thousand-model picker
```

The following are not implementation items because Cypher already has an
equivalent or deliberately different implementation:

```text
7207f74  Stable account ordering
dc6b8e5  Codex quota window (Cypher has no corresponding quota UI)
f5fb9b9  Claude MCP OAuth/plugin preservation
989be0a  Sibling-dial backoff reset
da792da  Atomic new-chat restoration (test coverage may still be extended)
```

The upstream subagent-document/right-pane-tab series, Zeron release and
branding commits, App Store/TestFlight configuration, GPUI fork, ACP
retirement, and forced Codex permission policy are explicitly excluded rather
than placed in a batch.

## Complete chronological non-merge ledger

This ledger is intentionally exhaustive. It contains all 186 non-merge commits
in `9ab250c..upstream/main`, including release markers, landing changes,
formatting, tests, and fixes which do not warrant their own feature section.

```text
2026-08-12 b1214b6 fix(ui): make changes pane freely resizable
2026-08-15 2b4d333 changes: comment a diff line and stage it on the next prompt
2026-08-15 45d5562 transcript: a sent prompt shows its folded context as a pill, not bullets
2026-08-15 70a7a49 review: earliest side marker wins, one radius, less prose
2026-08-15 89aa28d changes: read a diff side by side, not just top to bottom
2026-08-14 c4e8cd4 fix composer reflow during panel resize
2026-08-14 5d20db8 adapt composer traits control to available width
2026-08-14 79d05d5 preserve minimum chat panel width
2026-08-14 2bd9ee3 format right pane resize changes
2026-08-15 f406c5b linux: draw caption controls under client-side decorations
2026-08-15 56ac020 review: comment-only steers, renames cite the right side, drafts stay put
2026-08-15 e2d6ea2 feat(proto): model checkout change request status
2026-08-15 7fb1158 feat(engine): resolve GitHub pull requests with gh
2026-08-15 3adc155 feat(engine): stream cached checkout change request status
2026-08-15 bdc8363 feat(ui): track pull requests for local and remote sessions
2026-08-15 72ab1d9 feat(ui): show pull request badges in sidebar and composer
2026-08-15 0c84d0d restore full right pane takeover
2026-08-15 2b157a0 feat(ios): support streaming device RPC
2026-08-15 ae70566 feat(ios): show host pull requests in session surfaces
2026-08-15 ede19ac test: cover pull request status across device boundaries
2026-08-15 0cb68c7 fix(ui): frost pull request tooltip
2026-08-15 46ddcf9 fix(engine): resolve gh from login shell path
2026-08-15 4f46337 fix(engine): preserve links on unsupported streams
2026-08-15 2998ca4 fix(ui): retry pull request watch after host upgrade
2026-08-15 819fc4c fix(engine): use origin for untracked branches
2026-08-15 048798d fix(engine): resolve uncached remote default branch
2026-08-15 60965ad fix(engine): evict inactive change request cache entries
2026-08-16 b9e90ce landing: point downloads at v0.2.3
2026-08-15 5bcf1f9 ios: single-flight token refresh, staggered room dials, frame-rate settle
2026-08-16 0e7c6e5 test(engine): isolate login shell gh path fixture
2026-08-16 390d6eb ios: reveal cached transcripts on layout-quiet instead of burning the settle budget
2026-08-17 2ca44ae shortcuts: ctrl+tab cycles through sessions
2026-08-16 74f4abe diff-sync: stop reconcile churn spawning git diff back-to-back forever
2026-08-17 a6f05de github: add FUNDING.yml so the repo shows a Sponsor button
2026-08-17 76b49f0 registry: gate default agent enablement on the installed probe
2026-08-17 aacb621 ui: handle empty agent catalogs safely
2026-08-17 4daf42e landing: add nine new tweets to the marquee
2026-08-17 579678d harness: native Claude driver over stream-json (A1)
2026-08-17 42a7303 net: cap mobile warm dials, overlap checkpoint with backfill, flush sends at state
2026-08-17 7129b50 ios: checkpoint downloads survive redials, registry single-dial, registry-first dial gating
2026-08-17 22b5a67 sync: pull-first HTTPS transport — 1-RTT bootstrap, WS demoted to enhancement
2026-08-17 3bea7a5 sync: audit fixes — heal waits for known server state, iOS retires pushes only on edge verdicts
2026-08-17 0ce7fc2 harness: native Codex driver over app-server JSON-RPC (A2)
2026-08-17 18b915e v0.2.4
2026-08-17 c7c7ce6 harness: Cursor driver via the pinned @cursor/sdk shim (A3)
2026-08-17 42d7161 landing: point downloads at v0.2.4
2026-08-17 60887f7 harness/engine: retire adapter machinery for claude/codex/cursor (A4)
2026-08-17 2133bae harness: Grok ACP hardening (A5)
2026-08-17 06da4c4 subagent visualization: per-subagent docs, spawn chips, right-pane tab (B)
2026-08-17 6cf87a3 ios: pull's checkpoint fetch must drain the socket's frame buffer
2026-08-17 3bb7d11 sync: pull-path failures are loud on both platforms
2026-08-17 2c9e306 sync: empty checkpoint frontier is NOT contained — fetch, never skip history
2026-08-17 786e2d6 sync: an encoded-empty frontier is a vacuous claim — fetch, never skip
2026-08-17 e8ca1f3 sync: cursor amnesty — a cursor above the checkpoint is re-verified, once
2026-08-17 8158901 v0.2.5
2026-08-17 bbe7bee landing: point downloads at v0.2.5
2026-08-17 bed83ba subagent viz: three live-rig findings fixed
2026-08-17 e713383 subagent tabs: contextual titles + top-aligned streaming runway
2026-08-17 0bd6a6b sync: pull/push hardening — bearer headers everywhere, body caps, bounded rows response
2026-08-17 7ac14e6 fix(ui): round project picker section corners
2026-08-18 7fd861e subagent UX: real terminal statuses, bare tab titles, designed chip
2026-08-18 0c31ec4 subagent polish: Agent chip label + codex message paragraphing
2026-08-18 06fe8d1 subagent tab: scroll follow + jump pill; chip quiets down; top fade
2026-08-18 1fdf149 shell: jump pills go glass
2026-08-18 1d30023 spaces: reach other drives from the add-project palette
2026-08-18 1eb0bef subagent tab: every open lands at the end
2026-08-18 8528e3b ui: let installed-only harnesses toggle off; picker requires an installed CLI
2026-08-18 dc6b8e5 accounts: correct Codex free-tier quota window to a month
2026-08-18 bca16a3 accounts: stop Codex add-account opening two identical auth tabs
2026-08-18 9a12547 subagent iconography: bot glyph for tabs and spawn chips
2026-08-18 4e0a89d ui: real frosted floats on Linux (wgpu backdrop blur)
2026-08-18 c6ad564 spaces: enumerate custom mounts and accept typed paths (PR #144 feedback)
2026-08-18 06c4706 v0.2.6
2026-08-18 17925d3 landing: point downloads at v0.2.6
2026-08-18 7e1eaea chat2: claim rooms on HTTP first-contact, not just WS join
2026-08-18 a46b701 grok: subagent visualization over ACP — lifecycle wire events + disk-tailed transcripts
2026-08-18 35791f2 shortcuts: label the alt modifier Opt on macOS
2026-08-18 e21d146 cursor: in-app Connect via the SDK's browser login + accounts integration
2026-08-18 077735e v0.2.7
2026-08-18 76ddfb6 landing: point downloads at v0.2.7
2026-08-18 bb90e24 cursor: live model discovery + typed model options actually applied
2026-08-18 9e5ee60 v0.2.8
2026-08-18 71b221f landing: point downloads at v0.2.8
2026-08-18 69778b7 cursor: isolate each SDK run's agent store — concurrent runs no longer lock
2026-08-18 d0ce010 v0.2.9
2026-08-18 a6db86d landing: point downloads at v0.2.9
2026-08-18 0213cac opencode: ACP harness with subagent viz off the sidecar event bus
2026-08-18 a00aa61 fix composer idle redraw loop
2026-08-18 3e777c4 send: worktree creation rides the durable command plane; relay calls get deadlines
2026-08-18 c06ed22 harness: native drivers rediscover slash commands
2026-08-18 102b37f v0.2.10
2026-08-18 fc111c1 landing: point downloads at v0.2.10
2026-08-18 7207f74 accounts: keep the list order stable — always
2026-08-18 479a341 harness: opencode probe example builds again post-worktree field
2026-08-18 6d00440 transcript: spawn chips link to the subagent tab, not an accordion
2026-08-19 48ff777 attachments: 10x chunks, parallel window, overall deadline, live progress
2026-08-19 a729ed4 attachments: progress ring on the sending thumbnail instead of label swap
2026-08-18 80a3f30 subagent tabs: working trailer + parent steer messages
2026-08-19 1f94405 codex: child userMessage items are steers, not delta-channel echoes
2026-08-19 54b2d45 subagent transcripts open with their prompt; opencode+grok forward user messages
2026-08-19 f4383e3 source control: resolve the default branch through gh, never git transport
2026-08-19 c53ecd1 send: New-worktree never silently degrades; undelivered sends say "Queued"
2026-08-19 989be0a doc_host: new-chat join backoff resets on sibling dial success
2026-08-19 266262a ui: contain wheel overscroll inside scrollable composer input
2026-08-19 271269c v0.2.11
2026-08-19 ed8cd60 sync: event-driven reconnects — online-bus everywhere, path monitor, tighter caps
2026-08-19 8f3bce8 attachments: ride the durable queue — QueueCommand first, bytes chase in drain
2026-08-19 449a1db relay: end-to-end peer-link liveness + registry-dark dial parking
2026-08-19 b98d698 ui: connection truth — pill, composer honesty, Queued badges (Phase 1)
2026-08-19 9055e01 delivery: peer-relay QueueCommand fallback + deterministic flaky-network suite (Phase 3)
2026-08-19 d4d4045 delivery: explicit failed state + retry, send-time timestamps (Phase 4)
2026-08-19 bebc058 style: rustfmt pass over the durability work
2026-08-19 454a1dd subagent steers: durable across harnesses (claude re-key, engine gates, opencode resume)
2026-08-19 ce35a1a gpui: repoint at wingleeio/zed post-merge; frost the change-request card
2026-08-19 7c6c123 opencode: surface provider failures instead of silent forever-Working
2026-08-19 296813f opencode: effort picker rides model variants — the wire had it all along
2026-08-19 bafcc4a v0.2.12
2026-08-19 061e6ec Calm connectivity truth: degrade hysteresis, staging sweep race, quiet pill
2026-08-19 3bfe306 Connection line: bare spinner, no surface
2026-08-19 9116ff6 Connection spinner goes grayscale (mini_mono_spinner)
2026-08-19 da792da Atomic new-chat sends: stage first, restore to canvas on failure
2026-08-19 7612382 v0.2.13
2026-08-19 c42f68b ios/sync: event-driven reconnect + relay link liveness (PR #168 port)
2026-08-19 6da274d ios: send truth, queued attachments, and worktree-on-drain
2026-08-19 446ffbf Retry mints a fresh attempt; dead commands terminalize instead of ghosting
2026-08-19 447b689 Composer: full-width @-mention panel, indexed file search
2026-08-19 abacb45 Expired undelivered sends are re-issuable too
2026-08-19 46de808 changes: keep an edit paired across no-newline markers
2026-08-19 7754391 chat2: cursor contiguity + gap repair — the new-session random-hang root cause
2026-08-19 b2b0b19 ios: configure App Store bundle identifier
2026-08-19 4769488 v0.2.14
2026-08-20 889b78e Fix terminal rendering after sidebar reopen
2026-08-19 f6911c3 ui: fix decorated ranges at soft wraps
2026-08-20 3536a37 Fix selection edge scrolling and terminal scrollbar
2026-08-19 e8f9e03 Attachment thumbnails: sending indicator back, no post-send blink, corners clip
2026-08-19 36cf9ef v0.2.15
2026-08-20 eda27e8 fix(acp): support Grok model switching
2026-08-20 fde9b4b fix: harden OpenCode model discovery
2026-08-20 0390a3d v0.2.16
2026-08-20 c3d2981 Thumbnail percent ring tracks the real relay transfer, not just staging
2026-08-20 c255f27 v0.2.17
2026-08-20 08e53cd ios/chat2: cursor contiguity + gap repair + retry re-issue
2026-08-20 7a05159 Keep agent spawn chips visible outside the tool-group collapse
2026-08-20 6530b88 Center chip header content inside the bordered card
2026-08-20 f5fb9b9 Preserve live MCP OAuth when switching Claude accounts
2026-08-20 a8ef0aa harnesses: no unrunnable fallback in the composer offer; uninstalled last harness can be disabled
2026-08-20 ac34d9f ios: clean up build warnings
2026-08-20 04b08ea v0.2.18
2026-08-20 5019dc1 Fix tool rendering: only agent calls with docs render as subagent chips
2026-08-20 18987da Background shell settlements must not bind subagent refs onto Run chips
2026-08-20 569793e Genus-gate subagent binding for every harness
2026-08-20 9c20793 v0.2.19
2026-08-21 181d667 An interruption marker is not a steer
2026-08-21 cb2f30d Spawn chips name the model their subagent runs on
2026-08-20 e744d55 Fix transcript viewport restoration
2026-08-19 89cfeef fix(ui): refine panel resizing and alignment
2026-08-21 0079972 fix(ui): gate Git add menu by workspace
2026-08-21 2761213 fix(ui): unify new-session action in titlebar
2026-08-21 be01c65 fix(ui): size project menu with sidebar
2026-08-21 a2db751 fix(ui): reverse right-pane takeover icon
2026-08-21 f8437ea Anchor live streams at the transcript end
2026-08-21 6f9f834 Add entry-level message copying and spacing refinements
2026-08-21 3a3fdca Fix transcript copy fidelity and spacing parity
2026-08-21 048f8a2 Fix viewport test row after upstream merge
2026-08-22 3fc7bb0 v0.2.20
2026-08-22 aa9f8bf Reasoning becomes a first-class transcript part
2026-08-22 bf63444 opencode: native HTTP/SSE driver replaces the ACP path
2026-08-22 0b421cc opencode: bound every HTTP call; a boot-window request parks forever
2026-08-22 705470c opencode: rustfmt the new driver module
2026-08-22 a401432 opencode: gate the first prompt on a live event subscription
2026-08-22 1cf7036 A double click in a text field takes the whole value
2026-08-22 03de7a5 v0.2.21
2026-08-22 4fd3557 opencode: the picker offers only connected providers' models
2026-08-22 dd10dbe v0.2.22
2026-08-22 0434895 Model picker scales to thousand-model catalogs; rows attribute their provider
2026-08-22 e2a2c9b v0.2.23
2026-08-23 fed1d64 Thinking rides the tool-group accordion
2026-08-23 66c7166 v0.2.24
2026-08-23 5306be2 Chat rows survive configs from newer peers; iOS learns opencode
2026-08-23 4aa0b5b v0.2.25
2026-08-23 1fc6843 Registry cursor can no longer jump over unapplied rows
2026-08-23 ab44937 v0.2.26
2026-08-23 28eb39b Orphan sweep waits for server truth; unreadable HTTP ack retries
2026-08-23 fb05600 v0.2.27
2026-08-23 92b5732 ci: add permanent internal TestFlight workflow (#213)
2026-08-23 8f62bcf TestFlight workflow: audience input for external distribution
```

## Maintenance

This file describes one immutable audit point. When refreshing it:

1. Fetch `upstream`.
2. Record the new upstream commit and date.
3. Recompute merge base, commit counts, and diff scale.
4. Audit new non-merge commits after the previous audit point.
5. Add capabilities to the grouped inventory and append every non-merge commit
   to the ledger.
6. Re-evaluate Cypher disposition against the current architecture.
7. Never merge or bulk cherry-pick former upstream merely to make this file
   shorter.
