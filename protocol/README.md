# Cross-language mirrors

Some wire codecs and pure logic exist in more than one language: the Rust
engine and clients, the TypeScript Edge Worker and the Swift iPhone client. This
page lists every deliberate mirror, the file that implements it in each
language, the test that pins it, and the shared vectors in
[`vectors/`](vectors/) where they exist.

**Rule: a change to a mirrored contract changes every implementation and its
vectors in one commit.** Each implementation's header cites this page.

## Mirrors with shared vectors

Every language loads the same JSON file and runs every case in it.

| Mirror | Rust | TypeScript | Swift | Vectors |
| --- | --- | --- | --- | --- |
| Registry merge core and HLC (`encodeHlc`, `hlcNewer`, `applyOp`, `maxClock`, `rowToSeedOp`) | [`crates/doc/src/registry/core.rs`](../crates/doc/src/registry/core.rs); tests [`registry/tests.rs`](../crates/doc/src/registry/tests.rs) (`shared_vectors_*`) | [`apps/edge/src/registry/registry-core.ts`](../apps/edge/src/registry/registry-core.ts); [`registry-core.test.ts`](../apps/edge/src/registry/registry-core.test.ts) | [`Sync/RegistryCore.swift`](../apps/ios/Cypher/Sync/RegistryCore.swift); [`RegistryCoreTests.swift`](../apps/ios/CypherTests/Sync/RegistryCoreTests.swift) | [`registry-core-v1.json`](vectors/registry-core-v1.json) |
| chat2 frame codec `[type u8][headerLen u32 LE][header JSON][payload]` | [`crates/sync/src/chat_frames.rs`](../crates/sync/src/chat_frames.rs) (inline tests) | [`apps/edge/src/chat/chat-frames.ts`](../apps/edge/src/chat/chat-frames.ts); [`chat-frames.test.ts`](../apps/edge/src/chat/chat-frames.test.ts) | [`Sync/ChatFrames.swift`](../apps/ios/Cypher/Sync/ChatFrames.swift); [`ChatFramesTests.swift`](../apps/ios/CypherTests/Sync/ChatFramesTests.swift) | [`chat-frames-v1.json`](vectors/chat-frames-v1.json) |
| Device-room frame codec `uleb128(len) ‖ header JSON ‖ payload`, header key order `s, k, to, from` | [`crates/rpc/src/device_room/frames.rs`](../crates/rpc/src/device_room/frames.rs); tests [`device_room/tests.rs`](../crates/rpc/src/device_room/tests.rs) | [`apps/edge/src/device/device-frame.ts`](../apps/edge/src/device/device-frame.ts); [`device-frame.test.ts`](../apps/edge/src/device/device-frame.test.ts) | [`Sync/DeviceRelayClient.swift`](../apps/ios/Cypher/Sync/DeviceRelayClient.swift) (frame codec); [`DeviceFrameTests.swift`](../apps/ios/CypherTests/Sync/DeviceFrameTests.swift) | [`device-frames-v1.json`](vectors/device-frames-v1.json) |
| Stream preview codec (inactive: the Edge relay is removed) | [`crates/sync/src/stream_preview.rs`](../crates/sync/src/stream_preview.rs) (inline tests) | — (the Edge rejects preview frames: `apps/edge/test/workerd/preview.workerd.test.ts`) | [`Sync/StreamPreview.swift`](../apps/ios/Cypher/Sync/StreamPreview.swift); [`StreamPreviewTests.swift`](../apps/ios/CypherTests/Sync/StreamPreviewTests.swift) | [`stream-preview-v1.json`](vectors/stream-preview-v1.json) |
| Preview reducer | [`crates/sync/src/preview_link.rs`](../crates/sync/src/preview_link.rs) (inline tests) | — | [`Sync/PreviewProjection.swift`](../apps/ios/Cypher/Sync/PreviewProjection.swift); [`PreviewProjectionTests.swift`](../apps/ios/CypherTests/Sync/PreviewProjectionTests.swift) | [`preview-reducer-v1.json`](vectors/preview-reducer-v1.json) |

The two preview files are also run without a simulator: the macOS stage of
`scripts/check.sh` compiles the iOS sources with
[`scripts/tests/stream-preview-vectors.swift`](../scripts/tests/stream-preview-vectors.swift)
and passes it both paths.

## Mirrors pinned by per-language tests

No shared file; each language's tests pin the behaviour, so a change updates
the tests on every side by hand.

| Mirror | Rust | TypeScript | Swift |
| --- | --- | --- | --- |
| Registry JSON wire frames (`hello`, `push`, `presence`, `probe` ↔ `state`, `rows`, `ack`, `presence`, `probe-ok`, `error`) | `crates/sync/src/registry/frames.rs`; `crates/sync/tests/registry_client.rs`, `registry_transport.rs` (`mock-server` feature), `registry_edge.rs` (against a running Edge) | `apps/edge/src/registry/registry-room.ts`; the `RegistryRoom` workerd tests in `apps/edge/test/workerd/` | `Sync/RegistryClient.swift`; `RegistryClientLifecycleTests.swift` |
| chat2 catch-up plan | `crates/sync/src/chat_client.rs` (`plan_catch_up`); `chat_client/tests.rs` | — (the server only reports state) | `Sync/ChatFrames.swift` (`chatPlanCatchUp`); `ChatFramesTests.testPlanCatchUp` |
| Room liveness constants (ping 15 s, silence lease 45 s, hello 15 s, probe 10 s, probe after 15 min quiet, backoff 250 ms to 30 s) | `crates/sync/src/chat_client.rs`, `crates/sync/src/registry.rs` | — | `Sync/RoomSocketLifecycle.swift` |
| Device-room relay kind `" relay"`, relay error codes, host liveness | `crates/rpc/src/device_room.rs` (`RELAY_KIND`, `HOST_OFFLINE`, …) | `apps/edge/src/device/device-room.ts` (`RELAY_KIND`, `HOST_LIVENESS_MS`); `device-room.test.ts` | `Sync/DeviceRelayClient.swift` |
| Transcript delta frames (`WatchDocMessages`) | `crates/doc/src/transcript_delta.rs` | — | `Sync/TranscriptFeed.swift`; `SideChatTests.swift` |
| Agent prompt envelope (comments, session references, translation markers) | `crates/proto/src/agent_prompt.rs` | `pi-runtime/extensions/cypher-translation.ts`; `cypher-translation.test.mjs` | `Models/Comments.swift`, `Composer/SessionReferences.swift`; `CommentsTests.swift`, `MentionsTests.swift` |
| Translation language aliases | `crates/engine/src/pi_translation.rs` | `pi-runtime/extensions/cypher-translation.ts` (`LANGUAGE_ALIASES`) | — |
| Notifications model (badge payload, preferences) | — | `apps/edge/src/notifications/notifications-model.ts`; `notifications-model.test.ts` | `Notifications/NotificationModels.swift`; `NotificationTests.swift` |
| Subagent aggregation | `crates/ui/src/subagents.rs` | — | `Models/Subagents.swift`; `SubagentsTests.swift` |

The iPhone app also ports desktop UI logic (markdown parser, theme, transcript
rows, slash menu, context ring, loaders); those files cite their desktop source
in their first line and are not wire contracts.

## Vector files

JSON, 2-space indent; byte strings are lowercase hex. A new version of a
contract gets a new file (`-v2`) rather than an edit that breaks older readers.

### `registry-core-v1.json`

An object with one array per function. Rows and ops are in their wire shape
(`{kind, id, seq, deleted, delHlc?, fields, clocks}` and
`{kind, id, op, set?, hlc, clocks?}`).

- `encodeHlc`: `{name, ms, counter, device, hlc}` — `encodeHlc(ms, counter, device) == hlc`.
- `hlcNewer`: `{name, a, b, newer}` — `hlcNewer(a, b) == newer`; `b: null` is
  a field never written.
- `applyOp`: `{name, row, steps}` — start from `row` (`null` = no row) and apply
  each step's `op` in turn. A step with `changed: true` must produce exactly its
  `row`; one with `changed: false` must return the input row unchanged (or no
  row). Each step's result is the next step's input.
- `maxClock`: `{name, row, hlc}` — `hlc: null` when the row has no clock.
- `rowToSeedOp`: `{name, row, op}` — `rowToSeedOp(row) == op`, and applying
  `op` to no row recreates `row` exactly.
- `convergence`: `{name, prefix, permute, row}` — apply `prefix`, then the
  `permute` ops in every order; every order must end at `row`.

### `chat-frames-v1.json`

- `types`: frame name → type byte; `maxHeaderBytes`: the header size limit.
- `encode`: `{name, type, header, payload, hex}` — encoding gives `hex`, and
  decoding `hex` gives back `type`, `header` and `payload`. Header keys are
  sorted, because the Swift encoder sorts them.
- `malformed`: `{name, hex}` — every decoder rejects these.
- `unknownType`: frames with a type byte nobody defines. The clients (Rust,
  Swift) decode them so they can skip future frame types; the Edge rejects them.
- `headerSize`: `{name, bytes, valid}` — a `hello` frame whose header
  `{"pad":"xx…"}` is `bytes` long decodes iff `valid`.

### `device-frames-v1.json`

- `frames`: `{name, header, json, payload, hex}` — `json` is the header's
  exact serialization (key order `s, k, to, from`, absent keys omitted);
  encoding gives `hex` and decoding gives back `header` and `payload`. The Swift
  client encodes from `json` because it builds header strings directly.
- `malformed`: `{name, hex}` — every decoder rejects these, including a sixth
  length byte (overflow, never a wrapped small length) and lengths past the
  buffer. Header shape is not shared: the Edge relays any JSON header, the Rust
  codec requires `s` and `k`.

### `stream-preview-v1.json`, `preview-reducer-v1.json`

Arrays. Stream preview cases are `{name, kind, header, text}` to encode or
`{name, hex}` to decode, with `valid`; reducer cases are frames
`{kind, header, text}` applied in order with the expected `display`,
`interrupted` and `replies`. See [the ephemeral stream design](../docs/design/ephemeral-stream-v1.md).

## Where the loaders run

| Language | How it reads the file | Runs in |
| --- | --- | --- |
| Rust | `include_str!` from the test module | `cargo test` (the `rust` and `macos` stages of `scripts/check.sh`) |
| TypeScript | JSON import in the vitest unit tests | `npm --prefix apps/edge test` (the `edge` stage) |
| Swift | `TestSupport.repoRoot()` + the path | `CypherTests` (the `ios` stage; `ios-tests.yml` runs on changes under `protocol/`) |
