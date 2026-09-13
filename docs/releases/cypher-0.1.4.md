# Cypher 0.1.4 — Side Chat and Identity

The Side Chat release: Cypher now opens a *temporary* chat straight from any
selected text — a settled quote in a transcript, a Git-diff pane, or a
terminal — without touching your sidebar, sync, or saved chats. Rounding it
out are real profile avatars, an honest device switcher, and the official app
icon everywhere the old geometric C mark used to be.

## What's new

- **Side Chat from selected text.** Any settled selection — in a chat
  transcript, a Git-diff pane, or a terminal — now offers a **Side Chat**
  action next to the existing comment pill. Picking it opens a temporary chat
  in the right pane, minted on the device that hosts the parent chat (all
  side-chat RPCs carry `targetDeviceId` and are relay-forwardable, so a Side
  Chat works even when the parent lives on a *remote* device).
- **Temporary until promoted.** A Side Chat is engine-owned and lives only in
  engine memory: no workspace `Chat` row, no public `WatchSessions` entry, no
  SQLite snapshot, no chat2 room, no run journal. It stays invisible to the
  sidebar and to sync until you promote it — there is no auto-send, and no
  durable trace while you experiment.
- **Bounded first-send context.** The settled quote is validated at start
  (non-empty, ≤ 64 KiB chars) and injected **in full** into the first send's
  effective prompt — never truncated engine-side. The engine also folds in the
  source context (the newest whole messages through the transcript anchor, or
  a diff/terminal surface label), bounded at 8 messages / 48 KiB chars, and
  treats all of it as *context*, never instructions. A failed first dispatch
  keeps the context for the retry; an accepted send consumes it.
- **The full chat experience, reused.** A Side Chat is the same transcript and
  composer you already know, driven by the same components — full markdown,
  selection/copy, comments-free by design (no nested Side Chats), inherited
  harness/model/reasoning/cwd/sandbox shown as read-only picker chips, and
  attachments carried on the first send.
- **Same-ID promotion.** Promoting turns the temporary chat into a normal root
  chat **with the same id and transcript** — persisted snapshot first, then
  workspace row, doc-handle flip, chat2 join, and public status backfill. The
  draft, attachments, harness continuity, and transcript all survive the
  promotion; the watch seamlessly hands off from the private side-chat status
  stream to the normal chat surface. Idempotent, too: a lost reply followed by
  a retry never double-promotes.
- **Old-engine feedback instead of silence.** If the device hosting the parent
  runs a Cypher engine older than this feature, the sidebar says so plainly —
  *"Side Chat requires a newer Cypher engine on the device hosting that
  session"* — instead of a generic RPC error. Unpromoted side chats that are
  simply abandoned are reaped after 5 minutes with no watchers (capped at 8
  unpromoted per engine).
- **GitHub/WorkOS avatars.** The signed-in profile now carries a real avatar
  (`avatarUrl` from the WorkOS profile, so GitHub avatars flow through),
  rendered with an HTTPS-only URL guard, a graceful initial-letter fallback,
  and refresh on profile updates. Older engines and absent URLs keep working —
  `avatarUrl` is optional on the wire and never assumed.
- **Truthful device presence.** The settings device switcher no longer paints a
  green presence dot on the current device — presence is now stated by real
  labels ("This device" / "You") instead of a dot that implied a live heartbeat
  it didn't have. The accounts and harnesses selectors were simplified around
  that same honest framing.
- **The official app icon.** The raster Cypher app icon replaces the old
  geometric C marks everywhere: desktop authentication/workspace gates and
  brand loaders, plus the iOS sign-in and new-session screens through the
  shipped `CypherAppIcon` asset. The legacy `cypher-logo.svg` glyphs are gone
  from the UI and the landing/favicon assets.
- **Compact UI polish.** Tighter composer and transcript chrome, narrower
  gutter rows for embedded panels, and other small visual tidy-ups from the
  Side Chat refactor.

## Tags & artifacts

Release tag: `cypher-v0.1.4` (immutable) — artifacts:
`cypher-0.1.4-linux-{x86_64,aarch64}.tar.gz`, `cypher-0.1.4-macos-arm64.dmg`,
`cypher-0.1.4-macos-arm64-app.tar.gz`.

The Linux artifacts build genuinely headless (`--no-default-features`: no
X11/Wayland/GPUI linkage) inside an Ubuntu 20.04 container and import no GLIBC
symbol newer than **2.31**, verified in CI before packaging — they run on
glibc 2.31+ hosts (Ubuntu 20.04/22.04, Debian 11/12, and newer). macOS ships
the default headed build on Apple Silicon, **ad-hoc signed and not notarized**
unless the CI signing secrets are configured (`MACOS_CERT_P12` /
`MACOS_CERT_PASSWORD` plus `AC_API_KEY_*` for notarization) — until then,
Gatekeeper may warn on first launch.
