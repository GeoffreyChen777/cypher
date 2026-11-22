# Files

The desktop's **Files** surface (the right dock's `+` → Files) is a file
manager over the selected session's checkout, on that session's host device —
a remote host works the same way as this machine, through the relay. The tree
sits beside the picked file; drag the divider between them to resize the tree
(double-click it for the default width; new Files tabs open at the last width).
Narrow panes show one at a time.

## The tree

- **Select:** click; ⌘-click adds or removes a row; ⇧-click selects a range.
  ⌘A selects every visible row.
- **Keyboard** (when the tree has focus): ↑/↓ move (⇧ extends), ← collapses
  or jumps to the parent, → expands or steps into the folder, Home/End, ⏎
  opens, F2 renames, ⌘C/⌘X/⌘V copy, cut and paste, ⌘D duplicates, ⌘⌫ moves to
  the Trash (⌥⌘⌫ deletes permanently), ⎋ clears the selection or a cut.
- **Context menu** on a row or the empty area: Open, Open with Default App,
  Reveal in Finder (this machine's checkouts only), New File/Folder, Cut, Copy,
  Paste, Duplicate, Copy Path, Copy Relative Path, Mention in Chat (a file chip
  in the session's composer), Rename, Move to Trash; on the background also
  Show/Hide Hidden Files, Collapse All and Refresh.
- **New file/folder** opens an inline name field in the selected folder (or
  beside the selected file). `a/b/c.txt` creates the missing folders; a new
  file opens for typing.
- **Rename** happens in place, the name's stem preselected. Clicking away
  commits; ⎋ cancels. Open files follow a rename or move, unsaved edits
  included.
- **Drag** rows onto a folder (or the empty area, for the root) to move them;
  hold ⌥ to copy. Drop files from Finder to import them, or copy them in Finder
  and paste with ⌘V. Names that are already taken are never replaced: copies
  become "name copy", imports "name 2".
- **Go to file:** the field above the tree searches the whole checkout (the
  same ranked search as `@` mentions); ⏎ opens the result and reveals it.
- **Git marks:** uncommitted files show M (modified), A (added or untracked),
  R (renamed) or ! (conflict), and folders holding changes are tinted. When the
  agent changes files the visible folders refresh by themselves, and open files
  without unsaved edits reload.
- Deletes ask first. The Trash is the host's (`~/.Trash` on macOS, the XDG
  Trash on Linux); a volume without one offers a permanent delete instead.

## Viewing files

- **Text** opens in the syntax-colored editor; ⌘S saves.
- **Markdown** and **SVG** open rendered, with a Source/Preview toggle.
- **Images** (PNG, JPEG, GIF, WebP, BMP, TIFF, ICO) show fitted to the pane or
  at 100%, with their format, pixel size and file size.
- **PDFs** open in the pane in WebKit's viewer (scroll, pinch to zoom, select
  and copy text) on macOS. A checkout on this machine is read straight from
  disk, at any size; another device's PDF is fetched first, up to 64 MiB.
  Refresh reloads it after the agent rebuilds it.
- **Other binaries** get an info card (type, size, modification time) with
  Show Hex, Open with Default App and Reveal in Finder; Hex shows the first
  64 KiB.

## From the chat

- **File links** in a reply (Markdown links to `src/a.rs#L12`, `a.rs:12`,
  `file:///…`) open the file in the session's Files tab, at the line.
- **File references** in inline code (`` `core/context.py:146-160` ``,
  `` `file.py:128` ``, `` `src/lib.rs` ``) show as chips: a grey file glyph,
  the file name in medium-weight code type at the inline-code size (0.86× the
  text, see [Code in prose](chat-appearance.md#code-in-prose)) and muted
  lines, on a faint neutral wash with a hairline outline, so a reference
  reads apart from plain inline code by its shape, not its color. The box is
  inline code's (1.25× the code size, centred on the name, 5 px corners); the
  wash is black or white ink at 3% with an 8% outline, deepening to 6% / 15%
  under the pointer. A chip that no longer fits its line moves to the next one whole;
  only one wider than the line breaks, a box per line.
  A span counts when it is one path-shaped token naming a file and either
  cites lines or has a folder; bare names without lines, globs, placeholders
  (`rank_<n>.pt`) and URLs stay code. Copying a chip copies the reference as
  written.
- **Hovering** a chip for a moment opens a preview of the cited lines with
  three lines around them, syntax colored, from the file as it is in the
  checkout now (not when the reply was written), on a frosted card that eases
  out of the chip. The card stays while the pointer is on it, so its code
  scrolls, selects and copies; it also has Copy (the resolved path and lines)
  and Open.
- **Clicking** a chip opens the file in the Files tab with the cited lines
  selected. A drag inside a chip (or a link) selects its text instead and
  opens nothing.
- The Pi runtime asks every model for this form by default (paths relative to
  the working directory, the whole path every time, lines as a `:42-58`
  suffix; see [File references](../../pi-runtime/README.md#file-references)),
  so the lookup below is the fallback for replies that don't follow it.
- A reference is looked up at the checkout root first, then by the Go to file
  search for files whose path ends in it (agents often write paths relative to
  a package, or just the name). A bare name is first read as the one longer
  path of that name elsewhere in the same reply. When several files match, the
  card lists them instead of picking one; a path that isn't found or lies
  outside the checkout says so.

## RPCs

Every file RPC takes `{chatId, cwd, path}` (plus `targetDeviceId` to reach
another device) and follows the rules in
[iOS Files / Changes](ios-workspace-browser.md#file-rpcs): `cwd` must still be
the chat's checkout, paths are relative, and each component is opened from a
held directory descriptor without following links. `.git` stays unavailable.

| Method | Extra params | Does |
| --- | --- | --- |
| `ListWorkspaceFiles` | — | A folder's entries, now with `size` and `modifiedMs`. |
| `ReadWorkspaceFileChunk` | `offset` | 384 KiB of a regular file's bytes, base64, with its size and modification time. |
| `CreateWorkspaceEntry` | `directory` | An empty file (0644) or a folder (0755); never replaces anything. |
| `WriteWorkspaceFileChunk` | `offset`, `data` | Imports: offset 0 creates the file, later slices must land exactly at its end. 512 KiB per slice, 1 GiB per file. |
| `MoveWorkspaceEntry` | `to` | Rename or move without replacing (`RENAME_EXCL` / `RENAME_NOREPLACE`); case-only renames work. |
| `CopyWorkspaceEntry` | `to` | Copies a file or a folder tree: links stay links, `.git` and special files are skipped, APFS clones where it can. Measured first: at most 50,000 entries and 8 GiB. |
| `DeleteWorkspaceEntry` | `permanent` | Moves to the host's Trash, or removes the tree for good (links are unlinked, never followed). |

Mutations run on two workers of their own, so a long copy never holds up
listings. Copies and deletes get 120 s; everything else keeps the 8 s
deadline. Older engines answer `unknown method`, which the surface reports as
needing an update.

### Workspace chats

A workspace's own chat (the owner's `Main` thread, `BotChat::is_owner_chat`)
has more than one folder: its private folder (`<store>/bots/<bot id>`, the
default root) and the folder of each project its scope grants (`ProjectGrant`
All or Selected) on the workspace's host device. Engine side, as built:

- **`ListChatFileRoots {chatId}`** (forwardable; send it to the chat's host
  with `targetDeviceId`, like every file RPC) answers `[ChatFileRoot]`:
  `{spaceId?, name, path, checkoutId?}`. The first entry is the default root
  (`spaceId` absent; `name` is the workspace's name, or the project's for a
  session). Then the granted projects on the host, sorted by name: `spaceId`,
  the project's display name, its canonical `path` on the host, and its
  `checkoutId` when the project is a known git checkout. A session lists its
  own checkout only; a hidden workspace thread (plans, routines, peer answers,
  memory passes) lists nothing. A project whose folder is missing or does not
  answer within 2 s is left out. Older engines answer `unknown method`: fall
  back to the default root.
- **`rootSpaceId`**, optional on every file RPC above and on `SearchFiles`
  (with `chatId`): address that project's folder instead of the chat's own.
  `cwd` stays the chat's own `cwd` (the same stale-context check). Without it
  nothing changes. The engine refuses it for any chat but a workspace's own
  chat hosted on this device, for a project the scope does not grant or that
  does not exist (one message for both: "project is not available to this
  workspace"), and for a granted project on another device ("project is on
  another device; open it from a session there"). There is no path-shaped
  root: only a space id. Inside the root the usual rules hold unchanged
  (relative paths, no link followed, `.git` unavailable).
- **Git** is path-addressed already: `GetCheckoutDiff`, `GetCheckoutFileDiffText`,
  `ListGitHistory`, `ListBranches` and `ListRefs` take the root's `path` as their
  `cwd`/`repoPath`. `WatchCheckoutDiffs` now also follows every git project
  granted to a workspace hosted on the device, whether or not a session lives
  in it, and picks up scope edits as they land; match a root to its diff by
  `checkoutId`, else by `cwd == path`.
- Projects on another device are not offered: a workspace works directly only
  on its host's projects (as Pi's workspace roots, [§10.2](../design/agent-bots.md#102-direct-work-owner-threads-with-work-directly))
  and reaches the others through sessions.

Desktop side (`crates/ui`: `files/roots.rs`, `state/file_roots.rs`,
`files/root_picker.rs`):

- The chat's tile (the Botboard's chat card) lists its roots when it shows the
  chat, and again when the grant or the host's projects change. An empty
  answer, an error or an older engine leaves the workspace's folder alone.
- **Files** and **Git** show one root at a time. A picker at the start of the
  surface's header lists the roots (the workspace's folder with the workspace
  glyph, projects with a folder) and appears only when the chat has more than
  one root; Git lists only checkouts (a `checkoutId`, or a project found to
  have git), and with a single checkout its header names it without a menu,
  so Git never shows a project other than Files' without saying so. Files
  and Git follow one pick per chat: the root the owner picked or last opened
  a reference in, else the first granted project, else the workspace's folder.
  Git on a pick that is no checkout shows the first checkout. A commit opened
  from History stays on its project.
- **References** (code-reference chips and file links in replies): a relative
  path is tried in the workspace's folder first, then each granted project in
  list order (the chips also search every root for paths ending in it, and
  list several matches, each under its project's name); an absolute path only
  opens inside the root it lies in (the innermost one when roots nest). It
  opens in Files at that root and line, and that root becomes the pick. A
  relative link found nowhere opens in the workspace's folder (which then says
  it is missing); a path outside every root never opens in Files: one on this
  device is revealed in Finder.
- Git's Branch scope names a project root's branch `HEAD` (the chat row
  carries no branch for it). Git offers no Latest turn scope: the engine
  records a turn's start tree in the chat's own folder only, never in a
  granted project. The terminal runs in the workspace's folder.
