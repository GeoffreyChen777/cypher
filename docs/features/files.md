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
- **Other binaries** get an info card (type, size, modification time) with
  Show Hex, Open with Default App and Reveal in Finder; Hex shows the first
  64 KiB.

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
