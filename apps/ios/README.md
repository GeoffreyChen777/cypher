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
- **Boot splash:** a cold launch opens on the desktop's ASCII wordmark, which
  decrypts from scrambled glyphs into ink, then lifts away once sign-in is
  resolved and the workspace has its on-device cache or a live room (3s cap).
  "LOADING" appears only if it's still waiting after the decode. Foregrounding
  never shows it; Reduce Motion gets a static mark and a plain fade. Rigs
  (`-demo`, XCTest) skip it; `-splash` forces it.
- **Session-first Home:** every active session in one list, one tap from
  launch, under Now (running or waiting on an answer), Today, Yesterday,
  Previous 7 Days, Previous 30 Days and Earlier; each row names its project
  and device. Project chips across the top narrow it (then Quick chats and
  No project); a chip's long press, or **Project ›** under the chips, opens
  the project's own page with its archive. Search sits under the title;
  once a swipe up scrolls it away, a search button top left brings it back,
  focused. It matches every word against title, last message, project and
  device, looks past the time filter, and lists matching archived sessions
  too. The sliders menu filters by device and last activity and sorts (New
  activity, Name, Device, Date created), as the desktop sidebar's view menu
  does; saved on this phone only. The archived shelf sits folded at the
  bottom. New (✎) starts a session in the chosen project, else the latest
  session's, or in another project, a quick chat, or a new project.
  `Space`/`spaceId` remain the shared wire-schema names — no schema
  migration.
- **Pi-only:** new sessions use Pi. Existing non-Pi sessions remain readable
  but cannot be driven from the mobile composer. The engine's mock harness
  is test infrastructure, not a selectable agent.
- **Target-device catalogs:** installed/enabled Pi and available models are
  read from that project's engine. One composer chip shows the model and its
  thinking level ("Claude Opus 5.5 · High", with the provider's mark) and
  opens one card for both: a rail of provider chips (brand mark, name,
  count) opens on the current model's provider, that provider's models list
  a line each with their context size, and the chosen model's thinking
  levels sit in a segmented track pinned along the bottom (or a note when it
  takes none). Each pick writes the model and level together; a model that
  doesn't take the current level gets its default. No static Claude/Codex
  models or synthetic
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
- **`@` mentions:** typing `@` at the start of a word lists sessions (synced
  rows from every project and device, nearest and most recent first; never
  the current chat or side chats, archived ones once something is typed)
  above files (the host's `SearchFiles` over the chat's checkout, or the
  project/picked worktree for a new session). A pick becomes an `@name` chip
  that edits as one unit; the draft holds the desktop's strict
  `cypher-file:`/`cypher-session:` Markdown, so mentions read the same on
  both. Up to 3 sessions per message: each one's bounded transcript (the
  desktop's 8-message/48 KiB window, 96 KiB total) is read from this phone's
  synced copy and rides only the effective `agentPrompt`; a copy that never
  synced fails the send and keeps the draft. Files travel as paths only.
  Quick chats before their first send offer sessions only; side chats offer
  none. Sent bubbles show chips, not markup.
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
- **Context recovery:** a prompt the host sent again after a failed turn
  (`recovery` on its doc entry) is captioned **Restarted with context** (with
  the attempt from the second on) in the Steer label's place, with the
  desktop's restart glyph. A phone has no hover: tapping the caption opens the
  desktop card's contents in a popover (restart point, the trigger and where
  it matched with the text around it, the added context, selectable and
  scrollable past 220pt, and the model that wrote it). While a recovery runs
  (`recovery` on the session row), the status strip shows the spinner with
  "Writing context to retry…" / "Restarting with added context…", the elapsed
  time, the matched phrase and the attempt; a working run wins, and a row
  older than five minutes shows nothing. Both fields are read leniently: a
  malformed one drops only itself. Settings stay on the desktop. Demo: the
  **Context recovery** project has a session per state (writing context,
  restarting, recovered, out of attempts) and a live one that replays the
  whole flow when opened and again for each message sent in it.
- **Run progress:** a long turn's narrated progress (`progress` on the
  session row, the desktop working trailer's bar) fills the rest of the
  working strip: the desktop's painted bar — passed stages as dots, the
  current one as a breathing ring at the fill's end, the steps to come as
  faint dots, a glint along the fill — and the percentage. Readings glide
  into place; Reduce Motion jumps and stills the glint. A phone has no
  hover: tapping the strip opens the stage tips as one card, each row
  leading with its state — a green check for stages done (with where each
  began), a pulsing ring in the text colour for the one under way (with how many steps
  are left), a dashed grey circle for steps not started. Read leniently like `recovery`. Demo: the **Streaming
  veil** session replays a run of readings while it's open.
- **Composer focus:** the editor owns a native UITextView/delegate rather than
  relying on SwiftUI TextField's backing-view accessibility ID or FocusState.
  Empty focus and the first typed character expand the model/thinking toolbar;
  picker/search keyboards cannot expand it. Editor generations reject late
  focus/text callbacks after a send; marked IME text is preserved during view
  updates, and long drafts scroll within seven lines, clipped to the editor,
  with the expanded card's glass tinted so transcript rows underneath cannot
  read through the controls. Regression checks cover
  the session's safeAreaBar and real taps in a Release simulator build; these
  are not a substitute for confirming the fix on the affected physical phone.
- **Transcript rubber-banding:** automatic pin corrections yield during
  tracking (the first touch), dragging, deceleration and native/programmatic
  animation, then wait for 150ms of quiet scroll-offset geometry. Repeated
  touches invalidate older queued corrections; streamed content yields to the
  same native gesture/bounce window but may retarget its own spring. Negative
  bottom distance during a bounce is not treated as a broken/blank layout.
  Idle reflows and keyboard-end corrections remain enabled after settling.
- **Long transcripts:** a session loads from its end. Opening one reads,
  decodes and parses only its last 5 messages — message by message off the
  main thread, never the whole doc — so its length doesn't change what
  opening or a streamed update costs (3,000 messages on the simulator: ~30ms
  in the background and ~3ms of rows, where the whole transcript took ~1s,
  mostly on the main thread). Scrolling back reads the 40 above while the
  reader is still two screens away (at once, if those 5 are shorter) and
  shows them once the scroll rests,
  holding the row being read where it is on screen; **Loading earlier
  messages** marks the top until the first message (a tap or VoiceOver loads
  at once). The turn scrubber still reaches every round: it knows the prompts
  above what's loaded and reads down to the one it jumps to. Leaving the
  session drops the pages loaded on the way back. The reveal reaches the
  bottom by jumping to the last row's id, never to an estimated offset. Demo:
  `-big` (120 turns), `-huge` (600), `-turns N`; `-slow-pages` makes a page
  land 45s after it's asked for. A session this phone has never opened must
  still download its whole doc first (the host's checkpoint — megabytes for
  a long session); it arrives in URLSession's chunks, and the snapshot it
  leaves on disk is written off the main thread.
- **Session actions:** long-press a session row (or swipe) to Rename, Archive or
  Delete; the session's top-right menu has the same. Delete asks first, tells
  the host (`Mutate deleteChat`, which also removes its subagent children and
  purges the host's doc), then removes the rows here even if the host is
  offline; a quick chat's scratch folder is removed too. The desktop's
  "Delete worktree too?" follow-up is not ported.
- **Message strip:** under each settled message, the desktop's hover strip,
  always shown: the date ("Oct 11, 9:21 AM"), the model that answered (warning
  tone + tap for what was requested when another model answered), then fork,
  restart and copy. Fork/restart follow the desktop's gates (Pi root chat, not
  running, host online; restart never on the newest message); a blocked one
  stays dimmed and explains itself on tap. Restart asks first, naming how many
  messages it deletes, then calls the host's `RewindSession`; a prompt's text
  returns to an empty composer. Side chats show date and copy only.
- **Quick chat:** Home's New menu → Quick Chat starts a project-less session
  on a chosen online device (`CreateScratchDir` on the host, then a row
  without a space). Quick chats list with every other session as "Quick chat
  @ device", with a chip of their own; other project-less sessions get the No
  project chip.
- **Slash commands:** typing `/` opens the desktop's grouped menu over the
  host's Pi commands (`ListCommands`): headings (Conversation, Agent modes,
  Subagents…), one line per command with its glyph and a badge for what it
  controls — how full the context is, Fast mode / Scripts / orchestration
  On or Off, the goal's status, running subagents. The switches come from
  `PiSessionModes` on the chat's host, asked each time the menu opens; an
  older host leaves those badges off. With nothing typed it lists the
  desktop's default six (`/compact`, `/fast`, `/scripts`, `/goal`,
  `/orchestrate`, `/subagent-status`). The phone has no Settings → Commands,
  so a typed name also finds the host's other commands, except those that
  only configure desktop Settings. `/orchestrate` and `/goal` open their
  choices (picked or typed with a space) under what is in effect, the
  current one checked. Picking only fills the draft; commands are sent as
  ordinary text and skip the model-availability gate. The list scrolls
  within what's free above the composer.
- **Context arc:** the session's context usage (registry
  `sessions.contextUsage`, which can trail a live turn by ~20s) runs as a
  short 60° arc round the send button, as on the desktop: muted, amber from
  75%, red from 90%, filling from the bottom up. It shows in every composer
  state, the collapsed pill included (its right end leaves 12pt for it). The
  arc takes no taps; a long press on the send button shows the reading and a
  Compact action (`/compact` as its own run), offered only while the session
  is idle, and VoiceOver reads the reading on the button with Compact as a
  custom action. The `/` menu's /compact row shows the same reading.
- **Scripts:** a Pi codemode call shows as a Script chip named after the
  tools its script calls; tap it to read the script (80 lines, the rest
  counted). The calls the script made (`{script id}/{n}` part ids) hang off
  it on a guide rail, as on the desktop, and old transcripts nest too.
  `tool_search` shows as Find tools with its query, and tool groups count
  "ran 2 commands and 1 script".
- **Tool status:** every tool chip ends with the desktop's status icon — an
  arc turning while the call runs (still under Reduce Motion), a green check
  once it completed, a red cross when it failed. A chip spins for as long as
  its doc part is unresolved. The demo's streamed reply (`-stream`, or any
  send) opens with a script and its call so the spinner can be seen offline.
- **Fork / Side Chat:** select transcript text for "Edit in Fork" (before a
  prompt; its text returns as the draft), "Fork from Here" (after a reply) and
  "Side Chat" — a temporary chat about the selection, hosted in the parent
  host's memory and streamed over the device relay; closing the sheet discards
  it, "Open as Chat" keeps it. A side chat whose device link drops for good,
  or that sits unwatched ~5 minutes on the host, ends.
- **Files / Changes:** separate options in the chat's top-right menu open read-only remote directory
  navigation, UTF-8 file previews and working-tree Git diffs with per-file
  patches. Changes uses a continuous, collapsible list in a locally bundled
  Pierre Diffs viewer with line numbers, syntax/word highlighting and
  unified/side-by-side layouts. Unchanged code starts folded and loads from the
  same snapshot only when expanded. The target is this chat's host/checkout, never the phone or another
  device. Files requires the updated engine RPCs; old hosts show an upgrade
  notice. No editing, staging, commits or terminal fallback. See
  [workspace browser](../../docs/features/ios-workspace-browser.md) for limits and
  validation boundaries.
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
- **Math:** replies typeset LaTeX like the desktop: `$…$` and `\(…\)` in
  the line, on the text baseline; `$$…$$` and `\[…\]` centered on a row of
  their own, shrunk to fit a narrow screen. Prices ("$5 and $10") and code
  keep their dollars, and TeX that does not typeset shows as inline code.
  Formulas take the text color (appearance, thinking's muted tone, the
  streaming fade) and copy or quote as their TeX source.
- **Comment / quote for the next input:** select text **directly in the chat**
  using native long-press/drag handles, then choose **Comment** from the system
  selection menu (Copy remains available). User bubbles, assistant paragraphs,
  headings, list/quote text, table cells and multiline code blocks support
  in-place selection. A reply's consecutive paragraphs, headings and lists
  (lists holding only prose) are one selectable text, so a drag selection
  crosses them; code blocks, tables, quotes and rules are texts of their own,
  and a selection stops at them and never crosses messages. While a reply
  streams, its arriving block stays separate until the reply settles, so only
  that block re-lays out per frame. The editor shows the frozen selected
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
  A tap opens its session from any screen: the request is consumed the
  moment it resolves (never replayed later), and it waits out a push/pop
  that is still animating, including an interactive back swipe, before the
  navigation path changes. See
  [`docs/design/notifications.md`](../../docs/design/notifications.md) for delivery and
  account boundaries.

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
[`docs/operations/ios-release.md`](../../docs/operations/ios-release.md).

Builds with the Xcode pinned in [`.xcode-version`](../../.xcode-version), the one CI archives and tests with (the app targets iOS 26 and uses its Liquid Glass APIs).

```sh
cd apps/ios
xcodebuild -project Cypher.xcodeproj -scheme Cypher \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

Or open `Cypher.xcodeproj` in Xcode and run. Dependencies (SPM, resolved
automatically): [loro-swift 1.13.x](https://github.com/loro-dev/loro-swift)
(matches the engine's loro 1.13), [swift-markdown](https://github.com/swiftlang/swift-markdown)
(cmark-gfm: tables/strikethrough/tasklists — the same feature set as the
desktop's pulldown-cmark config) and [SwiftMath 1.7.x](https://github.com/mgriebling/SwiftMath)
(typesets LaTeX in replies; its math fonts add about 7 MB).

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
xcrun swift scripts/ios-icon.swift generate packaging/cypher.png \
  apps/ios/Cypher/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png
xcrun swift scripts/ios-icon.swift check packaging/cypher.png \
  apps/ios/Cypher/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png
```

Run these commands from the repository root.

### Connecting

- **WorkOS**: enter the edge URL, open the sign-in page on any device, paste
  the code it shows (`/auth/exchange`), pick an org (`/auth/refresh` re-scopes
  the token with the `org_id` claim).
- **Dev**: against an `AUTH_MODE=dev` edge (e.g. `wrangler dev`), enter a user
  id + org id; the bearer is `userId@orgId`. The `CypherDev` bundle
  (`scripts/dev-ios.sh`) connects to a local `wrangler dev` on
  `127.0.0.1:27640` by itself (see `docs/development/local-edge.md`).
- **Demo mode**: fully offline dataset with a scripted streaming reply —
  explore the UI with no infrastructure. Development builds also take
  `-mock-providers` (`scripts/dev-ios.sh -mock-providers`) to append mock
  Claude / ChatGPT / gateway providers to the real engine's catalog, so the
  grouped model picker can be exercised regardless of what is configured.
  Launch args for screenshot rigs:
  `-demo [-route chat:<id>|space:<id>] [-stream]`.
  Subagent inspector fixture:
  `-demo -route chat:chat-veil -sheet subagents`.
  Child fixture: `-demo -route chat:demo-child-planner`.
  Script fixture: `-demo -route chat:chat-deploy`.
  Math fixture (LaTeX in a reply): `-demo -route chat:chat-math`.
  Context recovery fixtures: `-demo -route space:space-recovery`; the live
  replay is `-demo -route chat:chat-recovery-live`.
  Session list view menu (sort, Last active): `-demo -demo-history` adds
  sessions from minutes to weeks old across projects, quick chats and
  project-less sessions.
  Comment editor/list fixtures:
  `-demo -route chat:chat-tabs -sheet comment` (or `-sheet comments`).

## Architecture

Source folders are features or layers; tests in `CypherTests/` mirror them.
The project uses synchronized groups, so adding or moving a file needs no
`project.pbxproj` edit.

```
Cypher/
  App/            entry point (CypherApp), AppModel and its data/session/
                  sign-in extensions, AppConfig
  Development/    demo dataset and its WorkspaceData conformance, launch and
                  bench rigs, Dev-bundle profile and interop probe
  Auth/           WorkOS/dev auth client, PKCE helpers, sign-in screen
  Notifications/  push registration and settings, payload models, banners
  Home/           session list, project sessions, archived shelf, row actions
  Session/        session screen, new-session flow, side chats, subagents,
                  comments, connection notices
  Workspace/      Files / Changes browser over the host's workspace RPCs
  Composer/       glass composer shell and draft, native text input, slash
                  and @ menus, model picker, question panel, attachments
  Transcript/     rows_for_entry port, transcript view, scroll state, veil,
                  turn scrubber, tool chips
  Markdown/       incremental parser, highlighter, block views
  Theme/          theme.rs port: palette, fonts, motion, icons, appearance
  Shared/         sheet chrome, loaders, boot splash
  Models/         wire and registry entity types
  Sync/           room clients and their shared socket lifecycle, chat2 and
                  device-relay codecs, registry/session stores, WorkspaceData,
                  doc persistence, stream-preview codec and projection
  Resources/      DiffRenderer.bundle (built from ../DiffRenderer)
```

`Sync/ChatFrames.swift`, `Sync/StreamPreview.swift` and
`Sync/PreviewProjection.swift` are also compiled standalone by CI against the
shared vectors in `protocol/vectors` (`scripts/tests/stream-preview-vectors.swift`);
keep them free of app-only dependencies and at these paths. The cross-language
mirrors and their vectors are listed in [protocol/README.md](../../protocol/README.md).

### Parity notes (desktop ⇄ mobile translations)

| Desktop | iOS |
| --- | --- |
| Sidebar: Projects + Sessions | Session-first Home: one list under day headings, project chips, search |
| Sidebar view menu (device, last active, sort, order) | Same options in Home's sliders menu |
| Horizontal session tabs per project | Project detail: vertical session list (the view menu's order) |
| Tab close = archive | Swipe-to-archive |
| Archived shelf under the sidebar list (open by default, Show-more paging, hover-swap Unarchive) | Same shelf under Home (folded) and project lists; unarchive is swipe-to-unarchive |
| Status word in the row corner (muted dots; Done keeps its pop; spinner rides bottom-right) | Same, same colors |
| Composer `white_alpha(0.03)` pill + hairline | Liquid Glass pill (`glassEffect`) + hairline |
| Harness brand SVG marks (icons.rs) | Same path data via a native SVG path parser (`BrandMarks.swift`) |
| Pi model picker + target-device catalog | Pi-only live catalog; one model chip and card with a pinned thinking track; no offline model fallback |
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

## Conventions

- One primary type per file; extension splits are named `Type+Aspect.swift`.
- Every app source file starts with a `//` comment saying what it is for; a
  port cites the desktop source it mirrors (`crates/ui/src/…`).
- Folders follow features (Home, Session, Workspace, Notifications, Auth,
  Composer, Transcript, Markdown) or layers (Sync, Models, Theme, Shared);
  rigs that only run in demo or development builds live in `Development/`.
- Stores are `@MainActor @Observable final class`; network transports are
  `actor`s; members are `private` unless something else needs them.
- Strict concurrency checking is `complete` in every configuration; fix
  diagnostics rather than suppress them, and justify any `@unchecked
  Sendable` or `nonisolated(unsafe)` in a comment.
- Sync never fails silently: log failures to `roomLog` instead of dropping
  them with a bare `try?`.
- Tests are named for behaviour; shared helpers live only in
  `TestSupport*.swift`; repository fixtures are found with
  `TestSupport.repoRoot()`, never by counting parent folders.
- Formatting is `swift-format` with `apps/ios/.swift-format`.
