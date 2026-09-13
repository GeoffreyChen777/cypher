# Fresh v3 rewrite — implementation ledger

## Current authorization

The user explicitly authorized a full, breaking v3 rewrite of all server and
client paths, with no old-data migration and no old-version compatibility.
This supersedes the legacy-data retention / conversion requirements in
`sync-v3-cutover.md`. Existing product features and execution-safety guarantees
are still required. A core test pass is not an end-to-end cutover.

Do not deploy or publish the intermediate worktree: its server configuration
and normal Engine path are not yet mutually compatible.

## Evidence recorded 2026-09-12

- Complete Xcode is installed at `/Applications/Xcode.app`. Use
  `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`; the system-wide
  `xcode-select` still points to CommandLineTools. No sudo is needed.
- The iOS app builds with full type checking. Early `swiftc -parse` results did
  not establish that it compiled; the wake/property collision and projection
  optional-type errors were found and fixed by the real build.
- Simulator: `786E6784-FC99-4C85-B37E-64DC42F24449`, Cypher Pi iOS Dev, iOS 26.5.
- Current signed simulator unit run: **191 passed / 0 failed / 0 skipped**,
  confirmed using `xcresulttool get test-results summary`.
  Logs: `/tmp/cypher-v3-full-ios-tests.log`,
  `/tmp/cypher-v3-full-ios-summary.json`.
  Run without `CODE_SIGNING_ALLOWED=NO`: disabling signing made the Keychain
  tests fail with missing entitlements.
- `cargo build -p cypher -q` passed with existing objc macro cfg warnings.
- Desktop development UI restarted: old PID 2902, new PID 88358. Verified cwd,
  data directories, log descriptors and Unix socket peer before restarting.
  Engine PID 65019 retained, local-only, data directory
  `/tmp/cypher-runtime-integration-engine`; other headless PID 37568 untouched.
  New UI startup log `/tmp/cypher-v3-ios-session-dev-ui.log` confirms connection
  to `/tmp/cypher-ipc-501/77b03c81e7dd8608042b1a59d43ea269/engine.sock`.
- iOS development app relaunched after tests, PID 88382. This confirms app
  startup, **not a working v3 connection to the normal Engine**.

## Implemented normal iOS session changes

- SessionStore no longer imports old snapshots, subscribes to Loro writes,
  starts chat2, or falls back to an old transcript on empty v3 state.
- Commands use the v3 SQLite outbox. Explicit nulls match the shared closed
  command descriptor. Only the optional empty RunRequest attachments field
  is omitted; nested model options and empty input answers are not altered.
- Journal filenames hash endpoint, organization, user, actor and room.
  Journal scope includes both organization and user. Storage failure is
  exposed, not silently reset or redirected to another transport.
- ACKed-but-unapplied command echoes survive reopen. Command owner epoch is
  read separately from log epoch. Regressions cover both.
- Request credentials are in Authorization headers, not URL query strings.
  Normal client has coordinated HTTP repair and fenced foreground replacement.
- Native JSON decoding keeps explicit tool resolution, input request ID,
  public artifact/diff metadata, committed message ordering and continuation
  joining. It no longer adapts v3 parts through LoroValue.

## Remaining required work (not completed)

- Normal Engine local/synced execution, durable intent/claim arbitration,
  persistent-process occupancy, raw SQLite event retention, producer
  publication and bounded UI/RPC transcript reads.
- Account-scoped WorkspaceHub: metadata, demand subscriptions, replaceable
  presence/view leases, bounded remote RPC and host wakeup.
- Normal iOS indexed bounded transcript windows (current normal projection
  still reads the complete projection), history loading, remaining UI status
  integration and recovery UX.
- Remove remaining static legacy decode helpers and their benchmark/test
  callers, then remove Loro/chat2 dependencies and unused transport code.
- Attachment bytes and artifact availability; normal input/control targeting,
  side chats, promotion, forks, subagents, worktrees and terminal/file routing.
- Transactional notification outbox, dedupe, account isolation and real-device
  notification verification.
- Snapshots, pruning, bounded replay/backpressure and full fault testing.
- Server cleanup: old implementations are still exported. The previous edits
  to production bindings/migration tags are incomplete and must be reconciled
  with the deployed DO migration history before any deployment, even though
  old user data does not need preservation.
- Actual normal Engine → Edge → iOS command and transcript convergence tests;
  existing core fixture/MockHarness tests do not prove normal-path adoption.
- Final full native/Edge/Swift suites, build artifacts, deployment/release
  checks and production verification. No production deployment has occurred
  during this rewrite.

## Engine replica/source milestone (2026-09-12)

Implemented `crates/engine/src/session_replica.rs` and
`crates/sync/src/sync3/local.rs`:

- A local authority assigns committed positions and applies the same sparse
  v3 reducer inside one transaction with receipt/outbox retirement.
- A persisted local-authority marker prevents a previously cloud-backed
  replica from acquiring local execution authority during network loss;
  the Engine cloud constructor also rejects local-authority databases.
- The Engine replica abstraction provides real WS + bounded HTTP repair,
  durable command intent advancement and source publication. Cloud claims
  wait for actual server history, not client-simulated ACKs.
- `ExecutionPublication` owns the non-copyable dispatch permit and retains
  raw events before producing public frames. Completion does not release a
  persistent process's occupancy; separate committed occupancy and release
  operations are implemented and tested.
- A new actual `MockHarness.run()` smoke uses the Engine replica and publisher,
  sends megabyte-scale Unicode/tool events through local workerd, then Swift
  independently verifies every normalized part by digest and bounded window
  paging. Restart observes settled execution rather than dispatching again.
  Healthy repair counts remain zero. Command:
  `python3 scripts/ci/sync3-smoke.py`; log
  `/tmp/cypher-v3-engine-smoke.log`.
- Rust Engine/sync/proto suites: 480 tests passed, five existing opt-in tests
  ignored (`/tmp/cypher-v3-replica-tests.log`). After adding occupancy coverage,
  the Engine publication tests and full cross-language smoke were rerun.

**Scope:** this establishes the replacement Engine storage/execution boundary
with real cross-runtime verification. Normal `DocHost::queue_command`,
`DocHost::execute` and `SessionsEngine::drive_run` still need to be wired to
it and their old writer/JSONL paths removed. It is not the full normal-client
cutover, and must not be reported as such.

## Normal harness-entry integration milestone (2026-09-12, subsequent)

- `EngineCore` now installs a runtime-scoped `SessionReplicas` store.
  Normal non-ephemeral `SessionsEngine::dispatch_inner` acquires a durable v3
  execution permit before invoking the harness. Routed run/steer requests
  carry their own publication context through the confirmed mailbox boundary.
- `drive_run` retains decoded source before filtering and publishes coalesced
  native parts through v3. User entries and finished assistant entries are
  present in the v3 projection of the normal Engine E2E test.
- The normal `queued_run_command_executes_end_to_end` regression now inspects
  that actual runtime's v3 journal: committed claim, completed run, matching
  message parts, private tool content in raw source and absent from public
  messages. This is stronger than the standalone replica smoke.
- Quiet parking rotates display messages without treating silence as proof
  the semantic run/process ended. Process occupancy release waits for stream
  closure and terminal reconciliation of unconfirmed mailbox commands;
  uncertainty is recorded, never automatically re-dispatched.
- Full Engine suite: **326 passed, 3 existing opt-in tests ignored**.
  Log: `/tmp/cypher-v3-normal-engine-tests.log`. Final E2E/steer rerun:
  `/tmp/cypher-v3-normal-final-tests.log`.
- Runtime retirement cancels replica transports and rejects later writes
  through retained handles. Private late source retention remains possible
  in the original account-scoped journal.
- Rebuilt/restarted the scoped development Engine (PID 26039) and UI (26085).
  Before restart, IPC showed local-only mode, no Working/AwaitingInput
  sessions, one known public chat, and no Engine child processes. Device
  identity and data directories retained; unrelated headless 37568 untouched.
  Startup logs `/tmp/cypher-v3-normal-dev-{engine,ui}.log`. Live IPC and Unix
  socket peer verified; one transient accept warning occurred while startup
  probing, with subsequent UI/CLI connections successful.

**Still not the complete cutover:** the outer DocHost command ledger, Loro
display writer and JSONL replay path still exist. Direct harness-entry
admission currently creates an inner v3 command; replacing the outer command
drain must preserve the original committed command ID instead. Worktree
materialization still happens in that outer executor, before the newly wired
harness gate. Input/interrupt command admission, ephemeral sessions/promotion,
autonomous post-Done public runs, and bounded reads remain required. In
particular, late autonomous observations are retained privately but do not
yet obtain a new public semantic run through the normal v3 publisher.

## Outer command queue/read cutover (subsequent 2026-09-12)

- Normal `DocHost::open` now uses `doc_host/v3.rs`, with no old snapshot load,
  chat2 join, migration sweep or snapshot writes. `ChatDocHandle::doc()` now
  exposes the native handle API, so existing Engine read/queue callers use
  v3 rather than accidentally reading the transient Loro render cache.
- Normal queue/drain preserves the original command ID. It uses committed
  projection state, rejects unowned/offline authority, and acquires the
  execution permit **before** worktree materialization. `dispatch_admitted`
  carries that same publication/permit into the runtime without a shadow
  command. Input/interrupt controls use their own committed control claim.
- User message publication moved after attachment resolution and before
  mailbox delivery, retaining final attachment paths and committed ordering.
- Recovery quarantines a lost v3 permit, closes streaming messages as aborted,
  records command rejection and preserves raw observations without inventing
  a harness Done. In-process reservations prevent direct dispatch and the
  background drainer from competing for the same command.
- A pending upload has a one-shot expiry wake; it no longer depends on another
  chat message or durable heartbeat. Unchanged transport ticks do not rebuild
  transcript watches. This is not yet a bounded indexed read implementation.
- Server-not-initialized is a structured, retryable condition for cold viewers.
  Native initialization runs within the transport actor, so queuing offline
  work does not require a network request first.
- Added `normal_v3_smoke`: actual EngineCore/DocHost/SessionsEngine through
  real workerd, followed by independent Swift full-output digest validation.
  `/tmp/cypher-v3-normal-core-smoke.log` passed; healthy repair count is zero.
- Latest focused E2E/publication/expiry suites:
  `/tmp/cypher-v3-queue-final-tests.log` (22 passed, one paid test ignored).
  Fork/attachment/workspace focused suites passed in
  `/tmp/cypher-v3-outer-focused.log`. Edge typecheck + 114 unit + 59 workerd
  tests passed after the cold-viewer change.

Remaining regression work includes two restart tests that still manufacture
legacy Loro/JSONL crash state and the real post-Done autonomous-publication
gap. The last full Engine run was **not all green**; focused results must not
be presented as full-suite acceptance. Old rollback salvage is intentionally
removed from the test contract, replaced by a no-legacy-import regression.
The legacy render-cache writer/JSONL path and unused old implementations still
need actual removal, along with the control/workspace/notification work above.

## Native recovery and autonomous output (Goal continuation 20)

- Provider session references are now indexed private v3 SQLite state, updated
  in the same transaction as source retention and queried by exact cwd.
  Resolved cwd is bound before dispatch/mailbox delivery. Normal resume no
  longer falls back to a Loro snapshot or JSONL scan. Replayed source and late,
  unclassified post-completion observations cannot overwrite the reference.
- Restart fixtures now manufacture actual v3 claims, raw events and bounded
  writer state. Recovery keeps partial text, adds a bounded interruption
  continuation, never synthesizes provider Done and never reissues a permit.
- Added the typed `runObserved { runId, executionId }` operation to Rust,
  Edge and Swift. It requires an open process occupancy under the current
  owner fence; it creates no user command or dispatch authorization.
- Observation-only permits require retained source and canonical application
  (an ACK alone is insufficient). Output uses the same bounded writer, with
  scope/run/source checks and durable completion. Crash worklists are indexed
  and paged; orphaned observation runs are quarantined without releasing an
  unverified process or changing the original applied command.
- Normal runtime autonomous text/tool output now enters a new observed run.
  The previously failing `parked_self_continuation_folds_and_requiesces` passes.
  Verified process closure awaiting remote run commits is retried on those
  commits, rather than waiting for a later user command.
- Real-workerd normal-Engine smoke now runs one process through the initial
  turn plus large Unicode autonomous output, verifies both semantic runs,
  one original command and actual process closure, then has Swift independently
  verify the complete output digest. Healthy HTTP repairs remain zero.
  Evidence: `/tmp/cypher-v3-observed-live-smoke.log`.
- Latest full proto/sync/engine test run:
  `/tmp/cypher-v3-observation-final-rust.log`: **484 passed, zero failed,
  five ignored** (paid/manual tests are not claimed as executed).
- Edge: **115 unit + 60 workerd tests**, typecheck passed, including the shared
  observation lifecycle/fence vectors. `/tmp/cypher-v3-observation-edge.log`.
- Full signed iOS simulator tests: **191 passed, zero failed/skipped**;
  authoritative result `/tmp/cypher-v3-observed-ios.xcresult`, summary
  `/tmp/cypher-v3-observed-ios-summary.json`.
- Fixed a shutdown-test counting error: its fake Edge counted EOF-only TCP
  sockets as HTTP requests. It now counts only nonempty reads; the targeted
  shutdown test passed five repetitions and the full suite passed afterward.

This supersedes the previous full-suite failure status, not the remaining
whole-product migration list. In particular, legacy control/workspace and
render-cache/JSONL removal, bounded normal readers and production acceptance
are still required. Audit questions arriving before a routed steer boundary
as part of the remaining normal input/control targeting work.

Dev refresh after the final observation/index validation change:
`cargo build -p cypher -q` passed. Engine **19217**, UI **19255**;
same existing integration data directories and verified Unix socket peer.
No live agent tasks were present before the scoped restart; unrelated Engine
**37568** was retained. Fresh `EngineInfo` succeeded. Logs:
`/tmp/cypher-v3-observed-final-engine.log`,
`/tmp/cypher-v3-observed-final-ui.log`. The status probe produced one
`ENOTCONN` accept warning; subsequent real RPC and UI connection succeeded.
iOS Dev simulator app was relaunched (PID **7954**). No production deployment.

## Early-input regression and WorkspaceHub implementation

- Added a failing-first regression for a real bridge question arriving before
  the provider's steer confirmation. The question now publishes in an observed
  run, is answered through a committed v3 control command, and does not falsely
  settle the queued steer. InputResolved is queued before waking the provider
  so an immediate next boundary cannot overtake that event. All five
  `turn_quiesce` tests passed (`/tmp/cypher-v3-early-input.log`).
- Implemented the fresh account-scoped Edge WorkspaceHub, its authenticated
  Worker route and development binding. Contract: `docs/workspace3.md`.
  It has bounded transactional current-row pages/pushes, exact request-byte
  ACK hashes, non-durable presence/demand, auth expiry on existing sockets,
  and hibernation-safe RPC with signed per-call nonces, bounded attachments,
  input queues and reply credits. It never transfers execution ownership.
- Edge typecheck and **116 unit / 67 workerd tests** passed. New runtime-real
  tests cover rollback/paging, no presence/RPC SQLite writes, header/account
  isolation, demand, hibernation, forgery, stale responses, backpressure,
  capacity and unknown delivery after host loss.
- **Normal Rust/iOS clients still use their old registry/relay paths.** The
  new Hub is not yet integrated into the product, and its server-only tests
  are not end-to-end migration acceptance. Next integration must replace
  normal RegistryClient, HostRelay and LinkCache, remove reseed/old storage
  paths, and implement bounded RPC payload handling plus session/read leases.
- Do not deploy this intermediate Wrangler configuration. Its historical
  migration tags must be reconciled with deployed migration history at the
  final cutover; the user's no-data-retention decision does not make resetting
  Cloudflare's migration history valid. TestFlight scope remains existing
  testers only, with no App Store submission or new invitations.

Final verification for this increment: proto/sync/engine **485 passed, zero
failed, five ignored**, `/tmp/cypher-workspace3-rust.log`. The attachment
queue regression now waits for the terminal command commit rather than
incorrectly treating user-message publication as proof of completion.
Desktop build passed; Engine **46166**, UI **46200**, same integration data
and verified IPC peer/EngineInfo. No active agent tasks existed before the
scoped restart; unrelated Engine **37568** remains running. Startup logs
`/tmp/cypher-workspace3-dev-engine.log` and `...-ui.log`; one cancelled-probe
ENOTCONN warning, followed by successful actual RPC. No production deployment.

## Native Workspace3 journal/client (continuation 21)

- Added scoped SQLite metadata/outbox/HLC storage, bounded row windows,
  transaction/rollback and restart tests, explicit local authority and no
  legacy import/reseed. Added a generation-fenced native control actor with
  separate request deadlines and bounded queues.
- Workspace hello/welcome now include **both expected user and org**, checked
  against Worker-authenticated scope before sending pending metadata.
  A changed token cannot silently attach a cached workspace to another
  account. Native code also verifies the endpoint/path before connecting.
- Added header-based v3 `UrlProvider::request`. Normal native v3 conversation
  sockets and test examples no longer use `?token=`; old registry/relay URL
  paths are still pending removal. Do not extrapolate this to a completed
  legacy transport cleanup.
- `/tmp/cypher-workspace3-native-live.log` passed real workerd interop:
  native offline outbox, exact ACK, host demand, RPC credits, wrong-account
  rejection with pending data retained, retirement and restart. The existing
  normal Engine → workerd → Swift conversation test also passed with header
  authentication.
- Native library tests cover account/org rejection before upload, headers,
  no automatic RPC replay, stale generations, local/cloud separation,
  immutable pending requests, cursor/row atomicity and clock observation.

The normal Engine workspace and iOS Hub adapters are **not yet switched**.
Next work must wire the library into WorkspaceHost/remote RPC, move the pure
LWW model out of the legacy doc crate, and implement the corresponding Swift
store/control actor. Normal workspace mutation/cascade atomicity, ephemeral
session state/read leases and bounded application RPC payload handling must
be preserved, not replaced with a generic test-only frame pump.
Also audit captured expected-account checks on **conversation** HTTP/WS
initialization during token/profile replacement; the new workspace handshake
is not proof of that separate boundary.

Final continuation-21 evidence: **493 proto/sync/engine tests passed, zero
failed, five ignored**, `/tmp/cypher-workspace3-native-all-rust.log`.
Eight new native workspace tests include concurrent joined shutdown.
`scripts/ci/sync3-smoke.py` passed again after the final frame/deadline/
retirement changes (`/tmp/cypher-workspace3-native-live.log`).
Edge typecheck + **116 unit / 67 workerd** tests passed.

The old shutdown fixture's receive-count race recurred; it now records when
each one-request TCP connection was accepted and checks that none starts
after shutdown, rather than classifying a delayed read on an existing
connection as a new request. It still verifies graph retirement and observes
the subsequent retry window.

Desktop build passed and the scoped Dev Engine/UI were refreshed after
confirming no active agent tasks: Engine **85023**, UI **85057**. Same data
directories and IPC socket; actual EngineInfo succeeded, unrelated Engine
**37568** retained. Logs `/tmp/cypher-workspace3-native-dev-engine.log` and
`...-ui.log`; cancelled-probe ENOTCONN warning followed by working UI/RPC.
No production deployment or TestFlight upload in this increment.

## Continuation 22 — normal native workspace and explicit local import

- Normal `WorkspaceHost::open` now opens an endpoint/org/user/actor-scoped
  workspace3 SQLite journal. It does not read/import workspace2 or registry1
  snapshots. The old workspace room implementation is still dead code pending
  deletion; its existence is not a compatibility requirement.
- Domain mutations commit cascades and HLC/outbox allocation atomically.
  Current render-cache updates are bounded to received metadata keys. The
  portable LWW model still lives in `cypher-doc` and must move out.
- Activity is replaceable presence, not a durable sessions row. A 100-update
  test verifies unchanged metadata cursor/outbox. Peer leases drive device
  availability; connection-specific close cannot retire a replacement peer.
  Known deleted chats suppress activity; a not-yet-indexed local session is
  not silently hidden.
- Cooperative device unpairing blocks background metadata resurrection.
  Explicit fresh sign-in may rejoin. Native focus probes and connection status
  are wired. Demand wakes owned conversations via a weak callback, armed after
  runtime assembly. Retirement drains the metadata client and fences writes.
- Explicit **new local-profile → synced-profile import** copies canonical v3
  public entries idempotently, with transactional receipts and private provider
  resume references. It never copies provider raw events, old execution
  commands, legacy JSONL or a dispatch permit. Empty public messages survive.
  Failure to save an import receipt rolls all imported public frames back.
  Row-only import and marker/read-root semantics remain tested. This is a
  product feature, not an old-data conversion path.
- Normal boot no longer opens rooms or recovers actions from legacy JSONL.
  Native recovery authority remains committed intents and observations.
- Real workerd smoke now runs **two normal EngineCores**, not only a library
  fixture: sidebar metadata, live Working status, peer rename/archive and the
  original normal source execution/late continuation all converge. The Swift
  reader still verifies complete bounded output from that normal producer.
- Rust **496 passed / 0 failed / 5 ignored**:
  `/tmp/cypher-normal-workspace3-all.log`. One initial 240-second full invocation
  timed out; isolated quiesce tests and the complete rerun passed.
  Edge typecheck + **116 unit / 67 workerd** passed:
  `/tmp/cypher-normal-workspace3-edge.log`. Full real three-language smoke:
  `/tmp/cypher-normal-workspace3-live.log`.

This supersedes the earlier “normal Engine workspace not switched” statement,
not the remaining complete-migration gates. Native **remote RPC still uses
old HostRelay/LinkCache**, and Swift still needs the new Hub store/control
adapter. Incoming new-Hub calls currently return explicit unavailable rather
than pretending to execute. Application RPC fragmentation/credit/cancellation,
bounded normal transcript readers and cold read lifetimes, durable subagent
recovery/notification semantics, conversation captured-account fencing,
legacy dependency removal and deployed end-to-end acceptance remain required.
Do not deploy this intermediate worktree.

Desktop build passed: `/tmp/cypher-normal-workspace3-dev-build.log`.
After checking process cwd, logs, IPC and **no running sessions/subagents**,
the integration Dev Engine/UI were restarted: Engine **41400**, UI **41431**.
Same config/data directories; old metadata is deliberately not imported.
Actual Unix RPC returned the native local device and empty chat/session lists;
the native workspace3 SQLite file exists. UI log confirms the original IPC
endpoint. Startup logs contain no errors. Formal app **69020** and unrelated
Dev Engine **37568** retain their original start times.
Logs: `/tmp/cypher-normal-workspace3-dev-engine.log`, `...-ui.log`.
No screenshot rendering, production deployment or TestFlight publication.

## Continuation 23 — normal native bidirectional remote RPC

- Normal `EngineRuntime` no longer starts `HostRelay` or constructs a device-room
  `LinkCache`. Device-addressed RPC shares the existing WorkspaceHub. The old
  relay code/explicit test helper and old routing fixtures still exist behind
  a temporary `RemoteClients` abstraction and must be removed, not presented
  as normal-path compatibility.
- Added native host/caller adapters, a bounded lossless JSON/base64 codec and
  RPC-client bridges. Requests/results are at most 8 MiB, with 32 KiB raw-byte
  fragments, two in-flight frames per direction, bounded queues, exact
  offsets/length/end checks, and no application invocation from partial input.
  Oversized output fails explicitly. Null values remain distinct from absent
  RPC fields.
- Hub input credits survive hibernation without durable per-call writes.
  Host work is capped across callers, not just per caller; caller closure
  cancels original token-bound work. Auth/generation, terminal status, stream
  cancellation, first-response/partial-message/credit deadlines remain
  independent of execution authority. Transient RPC is never replayed.
- The native remote service rejects IPC-only methods and foreign target IDs.
  Host services are weakly held. Runtime disconnect retires transports and
  cancels active host/caller tasks. Native recovery of a transport preserves
  metadata outbox retry, not RPC retry.
- Real workerd smoke exercised normal IPC → Engine remote forwarding → native
  Hub → actual remote Engine file browsing: complete 250 KB Unicode file
  output, >64 KiB input reaching the normal SearchFiles validator, rejection
  of remote SignOut, and **12 repeated idle stream subscription/cancellation
  cycles** returning the caller route count to zero. Existing three-language
  source convergence and complete Swift transcript validation still pass.
- `/tmp/cypher-workspace3-rpc-all.log`: **526 passed / 0 failed / 7 ignored**
  across proto/sync/RPC/Engine. A first full invocation timed out while launching
  the integration binaries; the complete rerun passed.
  `/tmp/cypher-workspace3-rpc-edge.log`: typecheck + **116 unit / 69 workerd**
  tests passed. `/tmp/cypher-workspace3-rpc-live.log`: complete live smoke passed.
- Dev startup exposed a GPUI-only regression: subscription cancellation was
  spawning from the foreground executor, where no Tokio reactor is entered.
  RpcClient now captures its construction-time runtime handle. The new
  regression polls a subscription outside Tokio, then verifies cancellation.
  `/tmp/cypher-workspace3-rpc-ui-regression.log`: **31 RPC tests passed**,
  2 ignored, including that added regression. This is narrower post-fix
  evidence, not a second full-suite claim.

Desktop build succeeded. Process/cwd/log/IPC and no-running-task preflight
preceded the scoped Dev Engine restart (**64745**). The first UI crashed and
its log is retained at `/tmp/cypher-workspace3-rpc-dev-ui.log`. After the fix,
only UI was relaunched (**65524**), retaining that healthy engine. Corrected
startup log: `/tmp/cypher-workspace3-rpc-dev-ui-fixed.log`; actual EngineInfo,
WatchDevices and WatchSessions over the original private Unix IPC succeeded.
Formal app **69020** and unrelated Dev Engine **37568** were unchanged.

Still required: Swift workspace/control journal and adapter, portable LWW
extraction and legacy fixture/dependency deletion, bounded normal transcript
readers/read lifetimes, notification/subagent recovery semantics, captured
conversation account fencing, attachment/artifact paths, and full deployment/
client acceptance gates. Do not deploy this intermediate mixed-client tree.

## Continuation 24 — portable Swift workspace storage/transport

- Added Swift workspace3 strict wire validation, SQLite journal, connection
  client and RPC fragment codec. They compile both in the actual iOS target
  and the standalone macOS live harness. They do not import old snapshots.
- Storage uses the same schema as Rust: immutable scoped identity, HLC +
  outbox transactions, exact-byte SHA-256 receipts, ACKs that never jump a
  contiguous cursor, observed remote clocks, current-row conflict rejection,
  bounded indexed optimistic windows and atomic local cascades. Receipt
  failure rolls canonical rows back with it. Fresh local-authority files
  cannot be rebound to cloud scope.
- Corrected Rust scope comparison to compare decoded, closed-schema identity
  rather than JSON key order. Real fixtures verify both Rust-created and
  Swift-created SQLite files can be reopened by the other language. No schema
  reset/reseed is hidden in that interoperability.
- Swift's reconnect actor validates exact endpoint/header auth and expected
  echoed account before uploading, uses replaceable presence/read interest,
  independent deadlines and bounded frame/control queues, and joins reader/
  timer shutdown. Wrong-account pending metadata stays on disk. Semantic
  conflicts park rather than regenerate state.
- Shared RPC corpus verifies Rust→Swift→Rust complete Unicode/null/bool JSON
  fragmentation, including UTF-8 splits. Large input gets a pre-encoding
  budget check; malformed, reordered or inconsistent fragments poison the
  decoder. Also guarded the old JSONValue double→Int64 conversion from a
  trap on out-of-range data.
- **Normal Swift WorkspaceStore and DeviceRelayClient are still old.** Next
  step must wire the shared context into normal metadata and remote calls,
  replace stale durable session rows with lease-scoped presence, remove
  create-space remote fallback/retry, make cascades atomic and surface storage
  errors without claiming phantom successful creates. The Subagents test's
  one `initialDocument:` seam needs a native fixture. SessionStore and
  Attachments must share the same account control connection, not create
  per-device sockets. Do not mistake this library work for that integration.

Evidence:
- `/tmp/cypher-workspace3-swift-full.xcresult`: **197 passed, 0 failed,
  0 skipped**, verified through `xcresulttool get test-results summary`.
  Full signed Simulator test run, not `swiftc -parse`.
- Final targeted iOS rebuild/run after send-age fencing: **6 passed**,
  `/tmp/cypher-workspace3-swift-final.xcresult`.
- `/tmp/cypher-workspace3-swift-live.log`: complete extended three-language
  workerd smoke passed, including two Swift workspace peers, offline writes,
  receipt/cursor correctness, read interest, presence, account fencing,
  retirement, both SQLite creation directions and both RPC codec directions.
  Existing normal Engine RPC/source and Swift full-output checks still pass.
- `/tmp/cypher-workspace3-swift-rust.log`: **9 native workspace unit tests
  passed**. An initial isolated `cargo test -p cypher-sync workspace3` failed
  because an old integration fixture requires `mock-server`; rerun used the
  correct `--lib` scope. No claim that those retired fixtures were migrated.

Desktop build passed: `/tmp/cypher-workspace3-swift-dev-build.log`.
After checking PID/cwd/logs/data dirs/IPC, only Dev UI was relaunched:
**70844**. Healthy headless **64745**, config and chats were retained.
Startup log `/tmp/cypher-workspace3-swift-dev-ui.log` is clean; actual
EngineInfo over the original Unix socket succeeded and the UI has its IPC
connection. Formal app **69020** and unrelated engine **37568** were unchanged.
No screenshots, deployment or TestFlight upload.

## Continuation 25 — normal Swift workspace/RPC adoption

- `AppConfig` now owns a lazily created account-scoped `Workspace3Context`.
  Normal sidebar, session read interest, file browsing, CLI controls and
  attachment handles share its one Hub connection. Normal `WorkspaceStore`
  reads/writes the native journal and uses a plain current-row cache, not a
  RegistryDoc snapshot/pending model.
- Added the production Swift unary RPC adapter and `WorkspaceRemote` facade.
  Input/results use the shared bounded fragment codec, window credits and
  captured generations. Admission is bounded even while awaiting connection;
  initial/partial/credit/overall deadlines are separate. No RPC auto-retry.
  Host errors, null results and uncertain delivery remain distinct. A remote
  handle's close cancels its own connection wait/calls, not the shared Hub.
  Config invalidation fences requests and retires shared work.
- SessionStore owns/releases a read-interest handle; command nudging no longer
  calls the old HTTP device endpoint. Workspaces use lease-scoped ephemeral
  statuses with matching actor/chat ownership and connection-specific closure.
  Fresh presence keeps long-running status visible without heartbeat WAL.
- Native sidebar mutations return no successful create ID on storage failure.
  Failures surface in the app's error banner. Parent/space deletes cascade in
  one local transaction. CreateSpace no longer tries a mutating remote call
  followed by a speculative local fallback. The native Subagents fixture now
  injects actual SQLite rows and replaceable presence.
- Deleted Swift RegistryClient, ChatRoomClient, DeviceRelayClient binary
  transport and DocDisk snapshot/saver implementation, plus their AppConfig
  query-token/HTTP registry/chat2/device helpers. App startup/logout no longer
  prunes or imports those retired formats. Loro decode/benchmark fixtures and
  legacy RegistryDoc tests still exist and need later cleanup/model extraction.

Verification:
- `/tmp/cypher-workspace3-app-final.xcresult`: **201 passed / 0 failed /
  0 skipped**, full iOS Simulator test target, checked with xcresulttool.
  Includes native create failure, descendant cascade, generation-specific
  presence, shared-context identity and facade-close cancellation regressions.
- `/tmp/cypher-workspace3-app-live.log`: complete extended live smoke passed.
  Production Swift RPC adapter connected through real workerd to the normal
  Engine; full 250 KB Unicode file, >64 KiB request reaching the actual
  SearchFiles validator, forbidden remote SignOut, and zero retained calls.
  Existing metadata, native execution, source output and three-language
  SQLite/codec/restart checks still passed. This uses the normal RPC adapter
  in a CLI harness, not a complete interactive iPhone acceptance run.
- Desktop build passed, `/tmp/cypher-workspace3-app-dev-build.log`.
  PID/cwd/log/config/IPC preflight preceded **UI-only** restart to **75357**;
  Engine **64745**, its chats/config and unrelated Engine **37568** retained.
  Clean startup log `/tmp/cypher-workspace3-app-dev-ui.log`, live UI IPC handle
  and successful EngineInfo. No installed application process was signaled;
  the former formal-app PID 69020 was no longer present at this audit.

Remaining full-goal gates are still open: bounded normal transcript readers,
conversation captured-account HTTP/WS fencing, notification/subagent lifecycle
semantics, native Rust model extraction and legacy code/dependency removal,
attachment/artifact completeness, broad feature parity/fault/performance
acceptance and production/client delivery. Notification HTTP still has a
`/registry/.../notifications/...` namespace; migrate and verify both ends.
No deployment, TestFlight upload or visual screenshot validation in this step.

## Continuation 26 — captured conversation identity and native notification routes

- Every normal Rust/Swift conversation init/exchange/WS request now sends
  the frozen `x-cypher-expected-user` assertion. Worker auth compares it before
  namespace selection. Refreshing a bearer cannot silently publish old
  pending state under another user. Normal Rust's captured user/org must also
  match the durable account tuple before opening SQLite or doing network work.
- New tests verify token rotation leaves the expected identity unchanged,
  malformed identity cannot become a header, and mismatched durable scope
  opens no file. Real workerd rejects a wrong-account `/init`; a subsequent
  correctly scoped probe confirms it left no initialized conversation behind.
- V3 data routes reject query transport. Native Swift HTTP/WS clients use
  non-redirecting sessions; auth/notification HTTP is bounded and nonredirecting.
  Scoped endpoint URLs reject embedded credentials/query/fragment.
- Notification HTTP moved from registry to WorkspaceHub on both clients and
  server. It reads native metadata; activity acknowledgement and semantic
  event writes are transactional. A real SQLite trigger proves an eventStatus
  write failure rolls eventState back too. Account-mismatched settings access
  is refused. Existing notification policy tests still pass.
- Replaced the production Worker entry point with native routes only. No
  legacy session/chat2/registry/workspace/device/blob implementation is imported
  or forwarded. In particular, obsolete attachment PUTs no longer receive a
  fake successful discard ACK. Those paths explicitly return 410; **native
  artifact mirroring still needs implementation/acceptance**, not a claim that
  rejecting its old endpoint finishes the feature.
- Production config removes RegistryRoom binding and schedules its deletion;
  this is configuration only, not an executed migration. Worker dry-run
  exports native bindings only. Bundle fell from roughly 552 KiB to 194.5 KiB
  by excluding retired implementations; this is not a workload-cost benchmark.

Evidence:
- Full Rust proto/sync/RPC/Engine rerun: **529 passed / 0 failed / 7 ignored**,
  `/tmp/cypher-v3-scope-notify-rust.log`. Initial invocation timed out before
  the quiesce binary printed its test list; isolated execution and full rerun
  passed. Do not claim a diagnosed OS cause for that launch delay.
- Later durable-account regression + nonredirecting auth client: **149 Engine
  unit tests passed**, `/tmp/cypher-v3-scope-notify-engine-final.log`.
- Final Edge typecheck + **119 unit / 70 workerd** passed; native-only dry-run
  passed, `/tmp/cypher-v3-scope-notify-edge-final.log`. No remote deployment.
- Final iOS simulator run: **201 passed**, `/tmp/cypher-v3-scope-notify-ios-final.xcresult`.
- Extended real-workerd Rust/Swift live matrix passed after final routing and
  transport edits: `/tmp/cypher-v3-scope-notify-live-final.log`.
- Desktop built. Verified Dev PID/cwd/logs/IPC and **no running sessions or
  subagents**, then refreshed Engine **5025** and UI **5056** with unchanged
  data/config and socket. New startup logs are clean; actual EngineInfo succeeds.
  Logs `/tmp/cypher-v3-scope-notify-dev-{engine,ui}.log`; unrelated Engine
  **37568** retained, no installed app signaled.

Still open: normal bounded history readers/checkpoint lifecycle; artifact
upload/availability; semantic-notification durable handoff, activity HTTP
heartbeat elimination and physical APNs acceptance; remaining Rust legacy
model/transport/dependency fixtures; full fault/cost/feature matrices; final
CI/platform artifacts/canary/TestFlight. Do not deploy the intermediate tree.

## Continuation 27 — durable native host uploads and real client readback

- Normal UploadChunk/UploadCommit now use the native private filesystem
  layout, not the old eight-character filename prefix. Full upload identities
  are hashed into filesystem keys, including case distinctions on macOS.
  Positional chunks are immutable; changed retries fail instead of overwriting
  data. Network RPC requires an explicit sequence number.
- Bounded base64 staging still accepts splits inside a base64 quartet. Commit
  streams through a verifying decoder, rejects trailing/invalid data, enforces
  the file-size limit, and builds bounded decoded read-block hashes. It does
  not accumulate the entire image in RAM.
- A private filesystem lock serializes independent upload handles/processes.
  Commit intents, fsynced files and immutable content-verified receipts make
  retries deterministic after restart. A lost receipt write cannot report
  success or expose an unreceipted native file. Repeated commits validate the
  original filename and file digest; reads verify touched blocks, including
  same-size corruption. Persist operations also sync newly created parent
  directory entries.
- Attachment sealing now propagates wrong-host, unresolved native ownership
  and durable outbox errors to UploadCommit. Its stable operation ID makes
  retries safe after the file itself is durable. This acknowledges the local
  durable seal enqueue, not a fictitious cloud-copy success.
- Unit coverage includes lost receipt persistence, restarted commit, changed
  chunk/name conflicts, full-ID and case-distinct paths, out-of-order chunks,
  arbitrary base64 boundaries, trailing-data rejection, cross-block reads,
  private file permissions and concurrent independent commit handles.
- The production Swift RPC adapter now performs a 120,003-byte upload through
  real workerd to a normal Engine, commits twice, reads all bytes back and
  rejects a changed committed chunk. The Rust probe waits for its attachment
  seal in the committed native conversation before fixing the Swift reader's
  expected head.

Evidence:
- Full Engine suite: **337 passed / 0 failed / 3 ignored**,
  `/tmp/cypher-v3-uploads-all-engine.log`. Updated filename assertions to the
  opaque native layout. Initial full invocation timed out before the final
  test binary printed its list; the complete rerun passed. No OS cause inferred.
- Final complete three-language/workerd convergence and restart matrix passed:
  `/tmp/cypher-v3-uploads-live-final.log`, including the normal Swift upload,
  repeated commit, byte-for-byte readback and committed attachment seal.
- Desktop build passed: `/tmp/cypher-v3-uploads-dev-build.log`.
  PID/cwd/log/IPC preflight verified empty WatchSessions and WatchChats, then
  refreshed the required Engine code and UI to **35117 / 35144**. Data,
  configuration, device identity and socket path were preserved. New startup
  logs are clean and actual WebSocket-over-Unix EngineInfo succeeds.
  `/tmp/cypher-v3-uploads-dev-{engine,ui}.log`; other Engine **37568** retained,
  no installed app signaled.
- Disk-full linker failure was recovered by removing only this repository's
  regenerable `target/debug/incremental` cache. Source, app data and test logs
  were retained. `git diff --check` passed.

This completes the **host-local** upload/readback step, not the full artifact
or migration goal. Cloud artifact mirroring/host-offline availability, bounded
normal history readers/checkpoints, native notification durable handoff and
activity-heartbeat removal/APNs, remaining Rust legacy/model/dependency
removal, full parity/fault/cost gates and final CI/platform/canary/TestFlight
delivery remain open. No remote deployment or visual screenshot validation.

## Continuation 28 — native metadata model extraction

- Workspace v3 metadata now has a storage-independent shared model in
  `cypher-proto::metadata` with strict `MetadataRow`/`RowOp` shapes and
  deterministic field-wise HLC merge semantics. The native Rust journal and
  WorkspaceHub use this model; the old registry model remains only for
  explicitly retired fixture code.
- Rust `MetadataView` is a disposable typed cache/draft. It has no snapshot,
  reseed, ACK or transport state. The SQLite journal remains the only place
  that allocates clocks and durably commits the outbox. Invalid rows are
  errors, not silently claimable chats; delete-space cascades emit only native
  metadata operations and no session/transport records.
- Edge has the corresponding strict `workspace3-model.ts`; WorkspaceHub,
  notification row reads and the new model tests no longer import the
  registry merge implementation. Swift has `Workspace3Metadata.swift` with
  strict Codable native row/operation types and merge tests; `JSONValue` was
  extracted as a general codec type, while the legacy registry fixture is
  test-only.
- Failure-boundary tests cover atomic SQLite rollback, disposable drafts,
  malformed ownership, equal-clock immutability, delete/revive races, optional
  clears, typed date/configuration round trips, concurrent field convergence
  and unknown/reseed-field rejection.

Evidence:
- Rust proto metadata + view tests: **7 passed**.
- Engine WorkspaceHost tests: **13 passed**.
- Complete Rust workspace/proto/doc/sync/rpc/engine run with mock-server:
  **626 passed / 0 failed / 7 ignored**, `/tmp/cypher-v3-model-rust-all-final.log`.
- Edge typecheck, unit and workerd suites: **121 + 70 passed**,
  `/tmp/cypher-v3-model-edge.log`.
- iOS simulator `CypherTests` after native Swift extraction: **204 passed /
  0 failed**, `/tmp/cypher-v3-native-model-ios.xcresult`, iPhone 17 Pro
  simulator iOS 26.5.
- Three-language real-workerd convergence/restart matrix passed,
  `/tmp/cypher-v3-model-live.log`.
- Desktop rebuilt and the scoped Dev App UI/engine were restarted after
  preflight. New PIDs are Engine **7092** and UI **7136**, with unchanged
  device identity/data directories and a verified EngineInfo over private Unix
  IPC. Unrelated Engine **37568** remained running. Logs:
  `/tmp/cypher-v3-native-model-dev-{engine,ui}-final.log`.

The complete v3 product rollout remains unfinished: the normal Engine opens
`SessionReplica`/native v3 journals when its production `SessionReplicas`
service is wired, while the retained `SessionDoc`/chat2 implementation is
still present for isolated legacy/fixture construction and must be removed
before the final direct cutover. Cloud artifact
mirroring and offline availability, bounded normal history adoption,
notification durable handoff/APNs, remaining old source/dependency removal,
full fault/cost/platform/release gates, production canary and TestFlight
delivery require further implementation and verification.

## Continuation 29 — notification activity is event-driven

- iOS notification coordination no longer starts a periodic 15-second HTTP
  activity heartbeat. WorkspaceHub presence remains the replaceable liveness
  lease; notification activity is sent only on semantic foreground/current-chat
  changes, with persisted sequence and generation fencing.
- iOS regression suite after this change: **204 passed / 0 failed**,
  `/tmp/cypher-v3-notification-activity-ios.xcresult`. This is simulator
  evidence; physical APNs delivery is still a release gate.

## Continuation 30 — bounded normal transcript reads

- `Sync3Journal.allMessagesBounded()` now reconstructs the normal iOS
  transcript through the indexed 32-message / 1 MiB window API instead of
  decoding the complete message table in one SQLite read. Each page is
  validated against its immutable `createdSeq`; pagination terminates on the
  exclusive creation cursor and preserves chronological order.
- `SessionStore.project()` uses this bounded reader for its rendered message
  projection while retaining commands, runs and attachments from the
  projection tables. A 75-message regression proves multiple bounded pages
  are lossless and ordered.
- iOS test suite after this change: **205 passed / 0 failed**,
  `/tmp/cypher-v3-bounded-ios.xcresult`.
- `cargo fmt --all --check` and `git diff --check` pass after the final
  native-model edits. Targeted Rust metadata/WorkspaceHost tests pass
  (**7 + 13**); final Edge typecheck, unit and workerd suites pass
  (**121 + 70**), `/tmp/cypher-v3-final-edge.log`.
