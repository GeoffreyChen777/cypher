# Cypher for iOS

A native SwiftUI **remote viewport** onto Cypher's connected Mac/Linux
devices. The phone mirrors the workspace registry and chat documents through
Edge, and drives the owning engine through the durable command queue.
**No engine, Pi Runtime, provider credentials, or MCP installation is required
on the phone.** TLS relay transport is used; this is not end-to-end encryption.

### Mobile scope

- **Appearance:** Home → account menu → Appearance offers System (default),
  Light and Dark. The choice is saved on this phone only. Surfaces, sheets,
  composer, Markdown and native selectable text adapt without changing layout.
- **Project-first:** Home lists projects with their owning device;
  open a project to create or resume its sessions, or access its archive.
  `Space`/`spaceId` remain the shared wire-schema names — no schema migration.
- **Pi-only:** new sessions use Pi. Existing non-Pi sessions remain readable
  but cannot be driven from the mobile composer. The internal mock E2E rig
  remains test infrastructure, not a selectable agent.
- **Target-device catalogs:** installed/enabled Pi and available models are
  read from that project's engine. No static Claude/Codex models or synthetic
  “Pi default” fallback. Empty/error states explain how to prepare **that
  device** using desktop Agents/Providers settings; the phone does not install
  Runtime. Retry, picker refresh, reconnect and foregrounding reload catalogs.
- **Continue, don't copy:** opening a desktop session retains its chat ID,
  host, working directory and model options. Model changes require an explicit
  available selection; unavailable configured models are not silently replaced.
  New-session model preferences are scoped by device.
- **Remote controls:** stream synced output, send/steer, stop (including while
  awaiting input), answer questions and attach photos. Offline devices remain
  browsable but new sends are disabled; drafts are retained on queue failure.
  A queued command is not proof that the remote run succeeded.
- **Quiet reconnect:** the chat's reserved bottom status row shows only a
  spinner while transport/model discovery reconnects. After 15 seconds without
  readiness, or a definite catalog failure, it shows one short notice + Retry;
  tap the notice for setup details. Returning to foreground/retrying renews
  the grace period. This is a display threshold, not a new transport timeout:
  automatic reconnect and existing command/draft protections are unchanged.
- **Steer bubbles:** explicit steer instructions remain right-aligned, with a
  small arrow/Steer label, softer fill, thin border and 14pt same-turn spacing
  (normal new exchanges retain 36pt). The synced, append-only command ledger's
  `steer` + `messageId` identifies materialized messages after reopening/on other
  iOS devices; optimistic echoes carry the same intent. The label is **not**
  an agent receipt, including when a steer falls back to a new run. Old messages
  without a matching command ID remain ordinary rather than being guessed.
  Demo's `chat-tabs` includes an example; no engine/schema change is required.
- **Device isolation:** changing the folder browser's device invalidates old
  requests/results. Creation is locked to the device that supplied the listing.
  Legacy orphaned sessions and their archives remain accessible.
- **Subagents:** a trailing accessory above the composer opens a live detail
  sheet (split running/starting/stale/done/failed counts, task, model, mode and
  bounded progress). It merges transcript tool parts, `Session.subagents` and
  durable `Chat.child` relations using desktop semantics. A successful async
  launch ACK is not completion; a heartbeat older than 45 seconds is stale,
  not finished. The child's own session status takes precedence.
- **Child navigation:** Open session enters the real child chat, where normal
  remote Pi controls remain available. Back/the parent arrow returns to the
  parent; nested children are supported. Child chats remain out of project
  lists, counts and root archives, but completed/archived children remain
  reachable through their parent. Links require a synced parent/child relation
  on the same execution device; absent links are shown as unavailable rather
  than guessed. The persisted agent profile is left untouched.
- **Comment / quote for the next input:** select text **directly in the chat**
  using native long-press/drag handles, then choose **Comment** from the system
  selection menu (Copy remains available). User bubbles, assistant paragraphs,
  headings, list/quote text, table cells and multiline code blocks support
  in-place selection. Selection is scoped to one text block/cell, not across
  separate messages or Markdown blocks. The editor shows the frozen selected
  quote and asks only for the comment — no second selection step.
  A selected live text block temporarily freezes its display to keep selection
  handles stable; it catches up on deselection while the rest of the session
  continues streaming. Fonts, syntax colors and rounded inline-code washes
  use the same native text layout as the selection.
  Save queues nothing: a pending-comments accessory lets you edit/remove
  annotations before the next normal Send/Steer. Existing typed drafts are
  not overwritten. Comments-only sends work; pending comments block slash
  commands. The visible prompt stays clean; the sibling command `agentPrompt`
  carries the desktop-compatible JSON quote/comment envelope.
  Draft comments are memory-only and discarded when leaving the chat or
  replacing the workspace. Queue/upload failure retains them; successful
  queuing consumes only the versions included in that send. Bounds: 32
  comments, 16k characters per quote, 8k per comment, 64 KiB annotation JSON.
- **Notifications:** Home → Notifications configures important events,
  per-Project muting and notification permissions. Alerts follow the session's
  last-used device. The app icon badge counts unread important-event sessions
  in this account; opening a session clears its contribution, Home does not
  clear everything, and logout clears the local icon. Registration, revocation,
  read receipts, badge revisions and notification taps are account-scoped.
  Real-device APNs delivery is a separate rollout/acceptance step. See
  [`docs/notifications.md`](../../docs/notifications.md) for rollout boundaries.

### Validation boundary

Simulator build/unit tests cover catalog filtering, failures, target changes,
cancellation and command-ledger serialization, in addition to auth and sync
conformance. Subagent tests cover wire/doc decoding, async ACK semantics, stale
heartbeats, durable children, root filtering, navigation and profile retention.
They do **not** establish production WorkOS login → remote Pi run
→ background/reconnect reliability or physical-device readiness.
Before distributing iOS, exercise those flows against connected current
engines, including two devices with different catalogs, missing Runtime,
provider setup, offline recovery, existing sessions and archived sessions.

## Build & run

For TestFlight/App Store preparation, signing and publication boundaries, see
[`docs/ios-release.md`](../../docs/ios-release.md).

Requires Xcode 26+ (iOS 26 SDK — Liquid Glass APIs).

```sh
cd apps/ios
xcodebuild -project Cypher.xcodeproj -scheme Cypher \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

Or open `Cypher.xcodeproj` in Xcode and run. Dependencies (SPM, resolved
automatically): [loro-swift 1.13.x](https://github.com/loro-dev/loro-swift)
(matches the engine's loro 1.13), [swift-markdown](https://github.com/swiftlang/swift-markdown)
(cmark-gfm: tables/strikethrough/tasklists — the same feature set as the
desktop's pulldown-cmark config).

Simulator tests that exercise the real Keychain require local ad-hoc signing:
use `CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=-` with `xcodebuild ... test`.
An unsigned test host (`CODE_SIGNING_ALLOWED=NO`) cannot access Keychain and
returns `-34018`; no physical device or portal provisioning update is needed
for these simulator tests.

Composer focus also has a real-tap regression test. Native
`becomeFirstResponder()` unit tests alone do not reproduce the iOS 26
`safeAreaBar` focus/layout issue. Run the separate UI-test scheme against an
isolated development simulator (it launches the app with offline demo data):

```sh
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
xcodebuild -project apps/ios/Cypher.xcodeproj -scheme CypherUI \
  -destination 'platform=iOS Simulator,name=Cypher Pi iOS Dev' \
  CODE_SIGNING_ALLOWED=NO test
```

Run this command from the repository root.

### App icon

iOS uses a dedicated **full-bleed, opaque RGB** icon. Do not copy the padded,
transparent desktop artwork straight into `AppIcon.appiconset`: iOS supplies
its own mask, and that padding produces an unwanted inset/border.
The generator preserves the existing design, crops the opaque face to an
undistorted square and fills its old transparent corners with the artwork's
own background color. It does not change macOS or in-app image assets.

```sh
xcrun swift scripts/ios-icon.swift test
xcrun swift scripts/ios-icon.swift generate dist/cypher.png \
  apps/ios/Cypher/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png
xcrun swift scripts/ios-icon.swift check dist/cypher.png \
  apps/ios/Cypher/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png
```

Run these commands from the repository root.

### Connecting

- **WorkOS**: enter the edge URL, open the sign-in page on any device, paste
  the code it shows (`/auth/exchange`), pick an org (`/auth/refresh` re-scopes
  the token with the `org_id` claim).
- **Dev**: against an `AUTH_MODE=dev` edge (e.g. `wrangler dev`), enter a user
  id + org id; the bearer is `userId@orgId`.
- **Demo mode**: fully offline dataset with a scripted streaming reply —
  explore the UI with no infrastructure. Launch args for screenshot rigs:
  `-demo [-route chat:<id>|space:<id>] [-stream]`.
  Subagent inspector fixture:
  `-demo -route chat:chat-veil -sheet subagents`.
  Child fixture: `-demo -route chat:demo-child-planner`.
  Comment editor/list fixtures:
  `-demo -route chat:chat-tabs -sheet comment` (or `-sheet comments`).

## Architecture

```
Sync/
  LoroProtocol.swift    loro-protocol 0.3 wire codec (byte-compatible port of
                        the crate's encoding.rs: magic/varBytes/type/payload)
  RegistryClient.swift  registry snapshot/ops relay, cursor and reconnect
  ChatRoomClient.swift  chat2 snapshot/row backfill, push/ack and reconnect
  DeviceRelayClient.swift  explicit target-device RPC over Edge
  WorkspaceStore.swift  devices/projects/chats/sessions registry mirror,
                        presence and optimistic viewer writes; the phone
                        publishes presence, not an engine-device row
  SessionStore.swift    session doc mirror: entries/parts (continuations
                        joined), command ledger appends (rule 1), host nudge
Markdown/
  MarkdownModel.swift   block model + incremental tail re-parser (re-parse
                        from the 2nd-to-last top-level block; link-defs force
                        full parses) — parser.rs port
  Highlight.swift       line tokenizer with carry state, paint-only
  MarkdownBlockView.swift  desktop metrics: body 14/22, headings 19/27…14/22,
                        code 12.5/18 (analytic line rows), violet inline code,
                        accent blockquotes, hairline tables
Transcript/
  TranscriptRows.swift  rows_for_entry port: block-granularity rows, stable
                        ids ({msg}#{part}.{block}, {msg}#g{n}), fingerprint
                        versions, consecutive-tool grouping
  TranscriptView.swift  lazy stack + stick-to-bottom (pin breaks only on user
                        scroll, 70pt re-engage band, 320pt jump button),
                        tool-group folds, error/input chips
  Veil.swift            paint-only streaming fade (EMA-tracked duration,
                        1−(1−p)^1.6 curve)
Composer/               glass pill, Send→Steer→Stop morph, QuestionPanel
                        (paged, numbered options, 220ms auto-advance)
Theme/                  theme.rs port: oklch→sRGB converter, exact palette,
                        Geist/Geist Mono, motion timings + flavour words
```

### Parity notes (desktop ⇄ mobile translations)

| Desktop | iOS |
| --- | --- |
| Sidebar: Projects + Sessions | Project-first Home, owning device, then sessions |
| Horizontal session tabs per project | Project detail: vertical session list (creation order) |
| Tab close = archive | Swipe-to-archive |
| Archived shelf under the sidebar list (open by default, Show-more paging, hover-swap Unarchive) | Same shelf under Home/space lists; unarchive is swipe-to-unarchive |
| Status word in the row corner (muted dots; Done keeps its pop; spinner rides bottom-right) | Same, same colors |
| Composer `white_alpha(0.03)` pill + hairline | Liquid Glass pill (`glassEffect`) + hairline |
| Harness brand SVG marks (icons.rs) | Same path data via a native SVG path parser (`BrandMarks.swift`) |
| Pi model picker + target-device catalog | Pi-only live catalog + reasoning chips; no offline model fallback |
| Add-project palette (device + folder browser) | New-project sheet: device tabs + remote folder browser (ListFolders over the device-room relay, git repos badged) |
| ControlRpc over device-room relay | `DeviceRelayClient` — binary `uleb128(len)+header+payload` frames, `{"s","k","to","from"}` header, ndjson ControlRpc; used for ListFolders + direct-to-host `Mutate {createSpace}` (local doc-write fallback when the host is offline) |
| Hover timestamps / copy | Context menus |
| gpui `list()` sum-tree virtualization | `LazyVStack` + stable row ids + version fingerprints |
| Stick-to-bottom spring, wheel-up breaks pin | Scroll-phase-gated pin + spring scrollTo, same 70/320pt thresholds |

Status colors, fonts, spacing, markdown metrics, veil timing, command-ledger
shapes, and the wire protocol are ports, not approximations — constants match
the desktop sources cited in each file header.

### Writer discipline (what the phone writes)

- Workspace registry: project/chat creates (host = the project's owning
  device), `archived`/`title`/`lastSeenAt` LWW sets, presence heartbeats.
  The phone does not claim to be an execution device.
- Session docs: command ledger appends only (`run`/`steer`/`interrupt`/
  `respondInput`), with client-minted message ids for optimistic echo. The
  host writes all transcript entries and command outcomes.
- After queuing a command it POSTs `/device/{host}/nudge` so a cold host
  opens the doc and drains — delivery stays durable in the doc regardless.
