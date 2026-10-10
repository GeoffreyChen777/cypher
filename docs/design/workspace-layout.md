# Workspace layout

The desktop shell has two columns: the sidebar and a freely tiled
**workspace** of sessions.

## Model

Two levels, and a tool never leaves its session.

```
Window
├─ Sidebar
└─ Workspace — free layout; the unit is a SESSION tab
   ┌──────────────────────────────┬─────────────────────┐
   │ [Session A] [Session C]      │ [Session B]         │  ← session tabs, projects may mix
   │ ┌────────────────┬─────────┐ │ ┌─────────────────┐ │
   │ │ Chat A         │Git│Files│ │ │ Chat B          │ │
   │ ├────────────────┴─────────┤ │ ├─────────────────┤ │
   │ │ Terminal (A)             │ │ │ Terminal (B)    │ │
   │ └──────────────────────────┘ │ └─────────────────┘ │
   └──────────────────────────────┴─────────────────────┘
```

- **Outer level (`workspace` module):** a tree of splits (any number of
  children per split, fractional sizes) whose leaves are **tile groups**.
  Each group holds one or more session tabs with one active. Presets: single,
  2 columns, 2 rows, 3 columns, 3 rows, 2×2, 3×3, two stacked + one,
  one + two stacked. Free layout by dragging a session tab to a group's
  centre (join) or edge (split).
- **Inner level (`SessionView`):** fixed structure per session — chat in the
  middle, a right dock (Git, Files, side chats), a bottom dock (terminals).
  Everything inside is bound to that session.

## Decisions

1. One workspace per window. The main window's is saved in
   `ui-settings.json`; a project window keeps its own, keyed by project.
2. Sidebar click opens the session as a new tab in the focused group (the
   most recently focused group holding a session) and activates it; if the
   session is already open anywhere, focus that tab. ⌘-click opens it in a
   new split to the right; drag places it exactly.
3. A session appears in at most one tab.
4. Side chats live in the session's right dock next to Git and Files.
5. Terminals live only in the session's bottom dock (the right-pane terminal
   surface is removed). It spans the whole session area — under the chat
   and the right dock — so a dock takeover never hides it.
6. When a session area is too narrow for chat + right dock, the dock takes
   over the tile (today's expand mode) with a toggle back.
7. Dock sizes are fractions of the session area, remembered per session; a new
   session starts from the most recent values.
8. Chat-scoped actions (⌘F, ⌘J, toggle right dock, next/prev session) act on
   the focused session. Seen-marking and attention cover every visible
   session.

## Implementation

Each visible session renders from its own `SessionContext`: a secondary
`AppState` pinned to the session (the project-window / side-chat fork
pattern), so `Transcript`, `Composer`, `Changes`, `FilesPanel` and
`TerminalPanel` keep reading `selected_chat` unchanged.

The main `AppState` runs in lists-only mode (`set_transcript_watches(false)`):
it owns the list watches and the sidebar; its `selected_chat` follows the
focused tile. Contexts mirror its lists on every notify and share its
send-in-flight map, so sidebar dots and chimes see sends from any tile.

## Persistence

- **Layout.** `ui-settings.json` `workspace` holds the main window's
  layout (these keys and `sessionDocks` load leniently: an unreadable one
  is dropped on its own, never resetting the other settings); `projectWorkspaces[projectId]` each project window's. Every
  workspace mutation (`Shell::workspace_changed`, split drags) schedules the
  debounced save. Only the main window writes the file: a project window
  merges its entry into the main window's copy (`Shell::persist_settings`),
  so it never clobbers other fields. Nothing is saved before boot landing,
  so a boot-time save can't overwrite the saved layout.
- **Pruning.** `Shell::prune_tabs` closes every tab — background ones
  without a slot too — whose chat is archived or out of scope (any notify),
  or missing from a NEW chats frame (the main state's chats generation;
  an optimistic insert or unrelated notify judges nothing missing). A
  missing chat stays while its row is on the way: a live pending send (a
  canvas's minted chat), its tab's composer still sending (a slow remote
  send outliving the 30s overlay), or a fork / promoted side chat this
  window created (`Shell::expected_chats`, 30s, cleared by a frame listing
  it) — `Shell::chat_awaited`. A tile's context that drops such a chat
  re-selects it. Pure decision:
  `tabs::session_fate`.
- **Restore.** Loading runs `Workspace::repair`. Once the first chats frame
  syncs, `tabs::restore_workspace` keeps session tabs whose chat exists, isn't
  archived and is in the window's scope; new-session canvases are dropped
  (a canvas holds only an unsent project pick). Emptied groups collapse,
  already empty tiles stay. Tabs opened before that frame (⌘N, the
  titlebar `+`) join the restored layout, focused (`tabs::adopt_presync_tabs`),
  and replace the landing. The old boot landing (latest session, or a
  canvas) runs only when no tab survives; a project window also opens the
  chat that moved with it. Main's selection follows the restored focus.
- **Lazy slots.** A slot (session context + views) exists only for an open
  tab, and is created when the tab is first on screen (active in a visible
  tile — a zoom hides the others), then kept until the tab closes. So a
  restored layout starts one context per visible tile, not one per tab.
  A slot leaving the screen (any workspace change) drops its transcript's
  comment pill/selection and the shared comment popup.
- **Docks (decision 7).** Right dock width and terminal height are
  fractions of the slot's session area, saved per chat id in
  `sessionDocks` with the open flags. A session without an entry starts from
  the most recently changed entry's sizes, docks closed. A dock never
  dragged falls back to the legacy global `rightPaneWidth` /
  `terminalHeight` pixels. Clamps: chat column ≥ 320px, dock ≥ 280px
  (`DOCK_MIN_WIDTH`), terminal ≥ `TERMINAL_MIN_HEIGHT` (≤ 55% of the area);
  too narrow for both, the dock takes the tile over. At most 200 entries
  are kept (least recently changed go first); boot drops entries of deleted
  chats and layouts of deleted projects.
- **Short tiles.** Height priority, top down: tile header > composer stack
  (never clipped) > terminal > transcript (may shrink to 0). The terminal
  shrinks to the room under the composer, below its tab bar + 48px
  collapses to the tab bar, and below that hides (still open; it comes back
  as the tile grows) — `dock::fit_terminal_height`. The right dock is
  unaffected.
- **Terminals.** Closing a tab parks its terminal panel in
  `Shell::parked_terminals` (by chat id; its context stops mirroring and
  watching): the PTYs keep running and reattach when the session's slot is
  created again. A deleted chat's parked terminals are closed on the chats
  frame that drops it (`Shell::prune_tabs`), which also drops its stashed
  draft (below).
- **Drafts.** Unsent drafts and staged attachments of closed tabs stay in
  memory (`Shell::closed_drafts`) and come back when the session's slot is
  created again. A background fork's prefill waits there too.
- **Composer defaults.** Every composer re-reads `composer-defaults.json`
  and applies only its own change before writing
  (`Pickers::update_defaults`), so tiles don't clobber each other's picks.

## Shortcuts

Rebindable in Settings → Shortcuts (Workspace group; stored in
`ui-settings.json` `keymap`, missing fields default):

| Action | Default |
| --- | --- |
| Split right | ⌘\\ |
| Split down | ⌘⇧\\ |
| Focus tile left / right / above / below | ⌘⌥← / → / ↑ / ↓ |
| Close tab | ⌘⇧W (⌘W stays Close Window) |
| Zoom tile | ⌘⇧↵ |

Fixed: ⌘1…⌘9 focus the Nth tile in reading order (unzooming another
tile). Ctrl replaces ⌘ off macOS. The tile verbs act only on the chat
route. View → Layout applies a preset (`shell::Layout*` actions).

## Deferred follow-ups

- A context mirrors main by comparing each list and notifies only on a
  change; transcripts rebuild rows only when the context's transcript
  revision moves. Per-list generations would skip the compares too, if
  profiling ever shows cost with many tiles.
