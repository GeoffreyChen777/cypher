# Cypher — Architecture

> "zeron" in this document refers to the original upstream implementation this
> product is a ground-up native rewrite and rebrand of; the current product is
> **Cypher** (binary `cypher`, crates `cypher-*`, Edge at
> edge.letscypher.app).

A ground-up native rewrite of the original zeron web app — a multi-device controller for coding agents
— in Rust, with a gpui UI. Pi is the only agent harness; chats from the retired Claude Code,
Codex, Cursor, Grok and Hermes harnesses stay readable but cannot continue.

**Pillars (from the goal):**
- Optional sync uses Loro CRDT docs (loro-mirror model) through Cloudflare Durable Objects; the same docs persist locally when sync is disabled.
- Durable Objects stay **TypeScript** (decision + evidence: `docs/research/durable-objects-language.md`).
  Everything device-side is Rust.
- Feature parity with zeron **except token-usage display** (poor fit for CRDTs; excluded).
  The one usage surface is the composer's context ring: a context-window gauge the host
  engine keeps on the chat's session-status row. It syncs through the registry like
  `subagents`, but sparingly: mid-turn readings ride the row's existing writes, and only
  a reading on a settled row writes on its own.
- Frontend is **gpui** (pinned Zed rev). Virtualization + markdown techniques ported from
  **mugen + pretext**.
- One binary, **headed or headless**. Smooth transitions/animations matching the original.

## 1. Topology (unchanged shape, new materials)

```
gpui UI ─ in-proc/Unix IPC RPC ─ engine A ══ DeviceRoom DO relay ══ engine B ─ RPC ─ gpui UI
                    │       optional edge Worker: auth, rooms, R2        │
                    └── optional chat2 sync ──  ChatRoom DO (per chat) ──┘
                                          └─ Workspace registry room ────┘
```

- **Engine = backend** (was `@cypher/backend`): runs agents, owns auth, terminals, repos/worktrees,
  diff sync, doc hosting. Pure Rust daemon, fully functional headless.
- **UI = viewport** (was Electron): gpui app rendering engine state. Talks the same typed RPC whether the engine is in-process or a separate daemon. Organized around **spaces** — (device, folder) pairs, local or synced according to the active profile. A window is two columns: the **sidebar** (the data — project cards with their sessions, sorted by activity, name, device or date) and a tiled **session workspace** (`docs/design/workspace-layout.md`): a tree of splits whose leaves are tile groups of session tabs. Each tab is a session with its own chat, a right dock (Git, Files, side chats) and a bottom terminal dock, all bound to that session. The workspace is a **device-local viewport** onto the list: closing a tab is local-only — archiving is an explicit sidebar action — and a sidebar click opens (or focuses) the session in the focused tile, ⌘-click in a split, drag anywhere. The layout persists per window (`ui-settings.json` `workspace`, `projectWorkspaces` for project windows) and dock sizes per session (`sessionDocks`, fractions of the session area). Every visible tile renders from its own **session context** — a secondary `AppState` pinned to that session — while the window's main `AppState` runs **lists-only** (list watches and the sidebar; its selection follows the focused tile). The new-session canvas carries a space picker (defaulting to the last selected space); new sessions are minted onto the picked space's device via relay-forwardable RPCs.
- **Edge (TypeScript, ported from zeron `apps/edge`)**: Worker + ChatRoom DO (per chat, the
  chat2 row protocol) + RegistryRoom DO (per user) + DeviceRoom DO (per device) + R2
  attachments + WorkOS JWKS auth. The retired SessionRoom class stays bound as a 410 stub so
  its stored legacy rooms are not deleted.
  Absorbs zeron's old `apps/server` responsibilities (WorkOS code exchange/refresh, orgs) so
  **Postgres, the Hono server, and the WebRTC/signaling stack are all gone**.

### Headed / headless
Single binary `cypher`:
- `cypher` — headed. If its engine is listening on the private Unix socket, verify its identity and connect;
  otherwise run the engine **in-process** (RPC over an in-memory duplex — same protocol, zero
  serialization shortcuts, so the boundary stays honest) **and serve that same engine over Unix
  IPC**. Other same-user viewports can attach without restarting the desktop app as a daemon.
  Binding and identity failures fail closed; an unrelated listener is not permission to embed
  a second engine. Endpoints derive from UID and canonical data directory, with private
  permissions, peer-UID checks and the `cypher.rpc.v1` subprotocol.
- `cypher headless` — engine only. A clean installation immediately serves its local profile over Unix IPC; when a saved account selects the synced profile at startup and a bearer is available, it also hosts its DeviceRoom for remote control. A VPS can run this while a laptop's UI drives it.

See [Unix IPC](docs/design/unix-ipc.md) for endpoint ownership, service names, Pi bridge,
and separate UI/Engine data directories. TCP IPC and its environment setting
have been removed; remote Edge transports are unchanged.

### Local-first workspace profiles

Authentication and workspace selection are deliberately separate state machines:

- `AuthState` is live credential state: `SignedOut`, `NeedsOrganization`, or `SignedIn`. It may change after login, refresh, revocation, or logout.
- `WorkspaceScope` is the immutable storage and transport boundary captured once at engine startup: `Local`, `Synced`, or explicit `Development`.

The engine never re-resolves an open store because `AuthState` changed. This prevents a sign-in, token refresh, or revocation from silently swapping databases or attaching online transports to a runtime that started local-only.

| Startup condition | `WorkspaceScope` | Online transports |
| --- | --- | --- |
| WorkOS enabled, no parseable saved `session.json` | `Local` | Disabled |
| Parseable saved WorkOS session | `Synced` | Enabled when a bearer is available; organization onboarding completes before opening the store when needed |
| WorkOS disabled without a dev bearer | `Development` | Disabled |
| Explicit non-empty dev bearer | `Development` | Enabled |

`cypher login` and `cypher logout` operate on `session.json` while the engine is stopped. Login selects `Synced` for the next start; logout selects `Local` for the next start. The UI may update live authentication status, but the active `WorkspaceScope` still changes only after restart.

The resolved profile selects the session snapshots, registry snapshot, run journals, and attachment cache that may contain workspace data:

| Scope | Store and journals | Uploads |
| --- | --- | --- |
| `Local` | `{data_dir}/profiles/local/` | `{data_dir}/profiles/local/uploads/` |
| `Synced` | `{data_dir}/orgs/{org_id}/{user_id}/` | `{data_dir}/orgs/{org_id}/{user_id}/uploads/` |
| `Development` | `{data_dir}/orgs/{org_id}/{user_id}/` | `{data_dir}/orgs/{org_id}/{user_id}/uploads/` |

The synced and development store roots preserve the historical cloud layout while their attachment caches are account-scoped. Local identity lives in `{data_dir}/local-profile.json`; its UUID is stable across restarts and is not an account or development identity.

Older releases wrote every synced and development attachment to `{data_dir}/uploads/`, and persisted those absolute paths in transcripts. On upgrade, the first synced or development account that opens this legacy cache claims it in `{data_dir}/legacy-uploads-owner.json`. That account may read the cache as a compatibility fallback, but all new staging and commits use its account-scoped uploads root; other accounts cannot read or write the legacy cache.

Device identity and machine resources remain device-scoped under the common data directory: `device-id`, repository registration, managed worktrees, agent credentials, and UI settings. They are available across profiles, but they do not contain or expose another profile's transcripts or attachments.

#### Privacy boundary and follow-ups

This first local-first change does not upload, import, link, or delete local sessions when a user signs in. Local attachments remain jailed under the local upload root and are not readable through the synced attachment cache. Returning to local-only mode reopens the same local identity and data.

The following product work is intentionally deferred:

1. Explicit session selection and copy between local and synced profiles, including attachment copying, provenance, and conflict behavior.
2. Browsing both scopes simultaneously or switching the visible scope without restarting the engine.
3. A supported self-hosted backend contract covering authentication modes, room APIs, authorization, persistence, and blob storage. Current endpoint and bearer overrides remain development/deployment seams, not a promised compatibility surface.

## 2. Data model — all Loro

Two persistent doc kinds. When sync is enabled, session docs ride the chat2 row protocol (loro updates as append-only rows + Range-resumable checkpoints, ChatRoom DO) and the registry rides its own row-frame protocol; local-only profiles persist the same docs without joining rooms:

1. **Session doc** (per chat) — the transcript + durable command queue. Schema is a Rust port of
   `packages/session-doc` (same container names/shapes): `meta` map, `messages` list (parts as list-of-maps with **LoroText bodies** — the
   measured 1.03× oplog shape; never LWW value rewrites), `commands` list with ledger rules 1–3
   (append-only per-device entries; host-only outcomes; dedupe/TTL/supersede evaluation).
   Continuation entries (`continuationOf`) are joined back into one message on read; tool parts
   are render-only (full inputs stay in the host's local run journal). Hosts publish neither a
   tail nor a diff sidecar ([chat2 sync](docs/design/chat2-sync.md)). Streamed assistant text
   commits every `STREAM_COMMIT_MS` (120 ms, `cypher-doc` constants).

2. **Workspace registry doc** (per profile) — the `registry1` snapshot stores spaces (id, deviceId, path, name?, gitDetected, checkoutId), the chats index (id, deviceId, title, archived, cwd, branch, checkoutId, spaceId, lastSeenAt, lastMessagePreview/At, config), devices, session-status rows, and checkout-diff summary pointers. A space is a device+folder pair in the active profile; the owning device's `SpacesSync` stamps git presence so branch pickers and the diff sidebar can gate without another RPC. Local scope keeps the registry entirely in its profile store. Synced and development scopes join `/registry/{orgId}/ws`, backed by the private per-user room `reg1/{orgId}/{userId}`; rows are never visible to every member of an organization.

   Writer discipline: each device writes its own device and session-status rows, rows for chats it hosts, and git stamps for spaces it owns. Creates, renames, archives, and seen marks are LWW sets accepted from any device. `deleteSpace` tombstones the space and every chat/session row in it in one commit. Presence uses ephemeral room frames rather than durable heartbeat writes.

   *Why one registry and not N tiny docs:* the sidebar needs one subscription for the whole list (grouping, resort animations, unseen markers). Its rows contain indexes rather than transcripts, so one local snapshot and, when enabled, one room connection remain bounded and cheap.

3. **Mirror layer** (`cypher-doc` crate) — Rust equivalent of loro-mirror: typed structs for the
   schema, **incremental** application of `doc.subscribe` diffs into cached state (no full
   re-hydration per change — this is also what fixes zeron's known O(transcript) re-projection
   inefficiency, remaining-work item 1a), and a hand-rolled diff-reconcile write path (the
   schema is small enough that no mirror library is used). The UI renders
   mirror state directly with per-entry change notifications — the "endgame" the TS
   implementation documented but never reached.

### Command plane
Send/steer/interrupt/respondInput = durable command entries in the session doc (`QueueCommand`),
executed by the chat's **host** device (executor gated on chat ownership; mark-processed BEFORE
execute; steer with no live run dispatches as the next turn). Offline sends queue in the doc.
This is zeron's proven design, kept verbatim.

## 3. Repository and crates

```
cypher/
  Cargo.toml  rust-toolchain.toml  rustfmt.toml  .editorconfig
  crates/
    env/          cypher-env      # every CYPHER_* variable, data directories, IPC socket paths
    proto/        cypher-proto    # wire types (AgentEvent, entities, RPC envelopes) + `view`,
                                  # the pure derivations both frontends share
    syntax/       cypher-syntax   # tree-sitter syntax highlighting
    doc/          cypher-doc      # session-doc + workspace-registry schemas, mirror layer, parts fold
    update/       cypher-update   # release checking + self-update (engine, CLI, UI)
    net/          cypher-net      # WebSocket dialing (happy eyeballs, proxies) + wake broadcasts
    sync/         cypher-sync     # Edge room clients (chat2, registry), presence, SQLite DocsStore
    harness/      cypher-harness  # Harness trait; Pi over its native RPC; mock harness
    rpc/          cypher-rpc      # typed RPC protocol, Unix IPC transport, device-room relay
    engine/       cypher-engine   # sessions, doc host, repos/worktrees, terminals, auth, RPC service
    ui/           cypher-ui       # gpui desktop app
  apps/
    cypher/                       # the binary (headed default, `headless` subcommand, CLI)
    edge/                         # TypeScript Worker + Durable Objects (apps/edge/README.md)
    ios/                          # SwiftUI iPhone client (apps/ios/README.md)
    landing/                      # letscypher.app landing page (static Worker assets)
    www-redirect/                 # www → apex redirect Worker
  pi-runtime/                     # curated Pi runtime bundle: extensions, patches, release.json
  packaging/                      # app icon, macOS Info.plist template, dmg art
  scripts/                        # dev-*, package-*, check.sh; ci/, tests/, ops/
  docs/                           # design, features, development, operations (docs/README.md)
  .github/workflows/              # CI, deploy and per-platform release workflows
```

`crates/harness/src/pi/engine-client.mjs` is the one non-Rust source inside a crate: the
harness embeds it with `include_str!` and hands it to Pi, so it lives beside its user rather
than in `pi-runtime/`.

### Crates and layering

Each crate depends only on crates in lower layers, dev-dependencies included;
`scripts/ci/check-crate-layers.py` holds this table and fails CI when a manifest breaks it.

| Layer | Crates |
| --- | --- |
| foundation | `cypher-env`, `cypher-proto`, `cypher-syntax` |
| model and transport | `cypher-doc`, `cypher-update`, `cypher-net` |
| clients | `cypher-sync`, `cypher-harness` |
| relay | `cypher-rpc` |
| engine | `cypher-engine` |
| desktop UI | `cypher-ui` |
| application | `cypher` |

The engine embeds everything below it; the desktop UI embeds the engine for in-process mode
and speaks `cypher-rpc` to a separate daemon. Conventions and checks:
[Development](docs/development/README.md).

Engine async runtime: **tokio** throughout; the UI bridges via `gpui_tokio` (`Tokio::spawn`
futures surfaced as gpui `Task`s). In-process mode runs the engine on an app-owned
multi-thread runtime (4 workers named `cypher-engine`, handed to gpui via
`gpui_tokio::init_from_handle`); the UI never blocks on it. Blocking work — subprocesses,
archive extraction, large directory removals, SQLite — goes through `spawn_blocking`
(`cypher_engine::off_runtime`), never a runtime worker: a blocked worker on a small
runtime stalls IPC, presence and sync together with no panic and no log line.

Loro hook rule: a Loro subscription (`subscribe_local_update`, `subscribe_root`) only
forwards data to a channel or a watch — it takes no engine lock and does no I/O. Loro runs
hooks synchronously inside commit/export on the calling thread and parks every other
thread committing to the same doc until the hook returns, so a lock inside a hook turns
contention into a cross-thread stall and re-entry into a self-deadlock (ACK → export → hook
→ client lock already held). The matching client-side
contract is on `ChatDocSink`: the chat2 client never holds its state lock while calling a
sink method that touches the document.

## 4. UI (gpui)

- **Deps**: `gpui` + `gpui_platform` pinned to one Zed rev (Apache-2.0). **We do not use Zed's
  GPL crates** (`markdown`, `ui`, `theme`, `editor`) — markdown, components, and theme are ours.
- **Transcript**: gpui `list()` + `ListState::new(n, ListAlignment::Bottom, overdraw)` (sum-tree
  offsets, follow-tail). On top of it, the mugen behaviors gpui doesn't provide:
  - stick-to-bottom **spring** with feed-forward tracking of streaming growth; interrupt from
    *user input* (wheel-up / drag), re-engage within a 70px band; own-send re-engages + smooth
    scrolls;
  - **block-granularity rows** (one row = one markdown block / tool group, not one message) with
    stable ids `msgId#blockId`; live turn stays unsplit, re-splits on persist; optimistic echo
    rows share the client-minted id so persistence never flickers;
  - row height memoization keyed by (row id, content length, width) so a streamed token
    re-measures one row;
  - scroll-anchor absorption for above-viewport height changes.
- **Markdown** (`cypher-ui::markdown`): `pulldown-cmark` parsing on `background_spawn` with
  coalescing (Zed's proven pattern), block-level incremental re-parse of the streaming tail
  (incremark's O(delta) idea: only re-parse from the last stable block boundary), monochrome
  theme where **numbers drive layout, colors are paint**. Code blocks: monospace, no wrap ⇒
  height = lines × line-height (layout independent of highlight); syntax highlighting comes
  from tree-sitter grammars in `cypher-syntax` ([Syntax highlighting](docs/design/syntax-highlighting.md)),
  colors applied as text runs (paint-only). Streaming **fade-in veil** on newly appended text via `with_animation`
  opacity (paint-layer, never affects layout). `prefers-reduced-motion` honored.
- **Composer**: hand-rolled gpui text input (derived from Zed's `examples/input.rs`: IME,
  selection, clipboard, key actions), compact↔expanded auto-flip by measured text width, auto-grow 76–260px,
  Enter/Shift+Enter, Send→Steer→Stop morph, drafts + attachments per chat, drag-drop/paste
  images, QuestionPanel (paged, 1-9 keys, 220ms auto-advance) replacing the composer while input
  is requested. Pickers (harness/model, traits, repo w/ folder browser, branch w/ worktree
  toggle) as gpui popovers with `menu-in` scale/fade.
- **Terminal**: `alacritty_terminal` (vte state machine, MIT/Apache) + `portable-pty` on the
  engine side; custom gpui grid element; tabs w/ drag-reorder (150ms sliding transforms), height
  drag 160px–55vh, 12ms input coalescing / 80ms resize debounce, 1MB replay, detach ≠ close.
- **Diff pane**: unified-patch parser → virtualized file/hunk/line rows, per-file collapse
  (180ms height tween), time-sliced highlight, 200ms width transition on the pane itself.
- **Animation kit** (`cypher-ui::motion`): small helpers over gpui `Animation` reproducing the
  zeron catalog — `fade-in` (0.5s, cubic-bezier(0.16,1,0.3,1), translateY 4→0), `splash-out`,
  `cypher-pulse` staggered cell wave (boot splash + loaders), `gradient-spin-pulse` matrix
  spinner (WorkingIndicator + rotating flavour word), `menu-in`/`dialog-in` scale-fades, 200ms
  ease-out width/height transitions for sidebar/panes, sidebar-resort **slide animation**
  (we own the list, so animate row positions directly — the View Transitions equivalent, 260ms
  cubic-bezier(0.22,1,0.36,1)), reduced-motion switch.
- **Theme**: always-dark monochrome, oklch-derived neutral scale precomputed to Hsla, hairline
  borders, Geist/Geist Mono bundled fonts.

## 5. Engine

Ports of zeron behaviors:
- **Sessions engine**: per-session broadcast hub; on-disk run journal (resumable `seq` replay,
  crash auto-resume); persistent steerable sessions (steering mailbox at step/turn boundary;
  30-minute idle reaper). There is deliberately no stall timeout: a live harness keeps the
  session fresh with a heartbeat, and a turn-quiesce watchdog parks a turn whose end never
  arrives; recovery stamps `aborted`.
- **Doc host**: per-chat handle (join room, VV backfill, write user entries + stream assistant
  segments at 120ms commits, drain commands host-only with processed-ledger idempotence,
  presence); a warm-doc LRU (`WARM_DOC_CAP` = 12 docs plus a byte budget) over a SQLite snapshot
  store; nudge-driven cold open.
- **Harness**: trait mirroring zeron's `HarnessShape`. The production harness is Pi, driven
  over its own RPC protocol (`docs/research/pi-rpc.md`); the mock harness backs tests and
  demos.
- **Repos/diffs**: the `git` subprocess (matches zeron, avoids libgit2 edge cases); worktrees
  under `~/.cypher/worktrees`; fs watchers (`notify`) + 2min repair; diff capture (patch +
  numstat + untracked, 3MiB cap, sha256) → workspace registry summary.
- **Auth**: WorkOS through edge routes (`/auth/exchange`, `/auth/refresh`, orgs); loopback
  callback server headed, paste-code headless; dev mode (no key ⇒ bearer = configured user id).

## 6. Edge (TypeScript, `apps/edge/`)

Ported from zeron's `apps/edge` (device room byte relay + nudges, R2 attachments, JWKS auth).
Its Loro-aware session room was replaced by the chat2 ChatRoom log relay
([chat2 sync](docs/design/chat2-sync.md)). On top of the port:
1. Private per-user registry rooms (`/registry/{orgId}/ws` → `reg1/{orgId}/{userId}`) with authenticated row sync and ephemeral device presence.
2. `/auth/*` routes absorbed from zeron's `apps/server` (WorkOS API key in Worker secret).
3. No `/seed` migration path or legacy sync (fresh app).
Hibernation hygiene: no idle timers (flush timer only while dirty), auto-response ping/pong —
per `docs/research/durable-objects-language.md`.
Source is grouped by feature (`src/auth`, `chat`, `registry`, `device`, `notifications`)
behind one ordered handler chain in `src/index.ts`; layout, conventions and the golden route
test are in [apps/edge/README.md](apps/edge/README.md).

## 7. Parity exclusions & deliberate changes

- **Excluded**: token-usage display (profile heatmap, lifetime stats, per-message token columns,
  `WatchUsage`).
- **Changed**: Postgres entity sync/server → workspace registry + edge; Electron/React/mugen → gpui with
  ported techniques; Node harness SDKs → subprocess protocols; WebRTC → device-room relay (zeron
  had already made this move); mobile app → native SwiftUI client (`apps/ios`).
- **Kept verbatim**: session-doc schema shape + constants, command ledger rules, edge DO design,
  render-parts privacy policy, UX behaviors and animation timings.
