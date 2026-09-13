# iOS read-only Files / Changes

Open the chat's top-right menu and choose **Files** or **Changes**. Each opens
its own sheet; there is no combined tabbed screen. The sheet is bound to that chat's
device, account/workspace instance and working directory, not the iPhone's local
filesystem or the project's main checkout if the chat uses a worktree.

## First version

- **Files:** directory navigation, filtering within the loaded directory,
  read-only UTF-8 text preview and explicit copying of loaded text.
- **Changes:** the existing working-tree diff capture (HEAD vs current files,
  including staged, unstaged and untracked changes), changed-file summaries,
  a continuous list of collapsible Pierre Diffs file cards with line numbers,
  syntax and word-level highlighting, and unified/side-by-side layout.
  Unchanged code starts folded and expands on demand. Raw patch is available in the
  diff menu or as a fallback. These are snapshots, not live
  editors; use Refresh to capture newer changes. They are not turn/commit or
  separately staged/unstaged views.
- Binary/non-UTF-8, empty, missing, permission-denied, offline and partial
  results have explicit states. No editing, staging, commits, terminal fallback
  or provider/LLM calls are performed.
- Tool chips still show their existing summaries; they are not historical
  file revisions or full tool-output viewers in this version.

## File RPCs

`ListWorkspaceFiles` and `ReadWorkspaceFile` take:

```json
{"chatId":"existing-chat","cwd":"/the/chat/checkout","path":"src/file.swift"}
```

`path` is a relative directory for listing (empty for root), or a nonempty
relative file for reading. The resolver reuses SearchFiles' workspace/linked
worktree validation, rejects foreign-device chat/space rows, and rechecks the
chat's device/cwd around asynchronous operations. Arbitrary paths do not
authorize themselves by appearing in a request or a tampered chat row.

On macOS/Linux, filesystem access walks from a verified canonical root using
held directory descriptors and `openat(O_NOFOLLOW)`. This rejects symlinks in
both the root's ancestors and relative traversal instead of a racy
canonicalize-then-open check. `.git`, `.` and `..` components, absolute/NUL
paths, non-regular files and symlinks are unavailable. Listings hide links,
special files, non-UTF-8 names and `.git`; other dotfiles inside the authorized workspace are
not treated as a secret-redaction feature.

Limits:

- 4 filesystem workers; permits remain held if a client times out while an
  underlying filesystem operation is still running.
- 8s RPC deadline; relative paths at most 4096 bytes.
- At most 1000 returned entries / 20,000 scanned directory entries, explicitly
  marked partial. Folder filtering covers only these loaded entries.
- 256 KiB file text preview, cut at a UTF-8 boundary. Binary or non-UTF-8 data
  in the loaded prefix does not produce a text preview. A detected in-place file change during read
  requires a refresh.
- Existing checkout patch capture remains capped at 3 MiB. iOS allows at most
  8 MiB per relay WebSocket message (including JSON escaping). The structured
  viewer displays at most 250 files, each at most 512 KiB / 10,000 patch lines;
  larger patches have a
  bounded raw-text fallback. Larger/partial data is not presented as complete.
- Expanding unchanged code reuses `GetCheckoutFileDiffText` with the native
  snapshot's checkout/path/checksum. Stale or partial snapshots cannot hydrate.
  Before/after source pairs are limited to 512 KiB combined and 10,000 lines
  per side, two concurrent requests and 4 MiB retained source per document.
  Both hunks and unchanged gaps must match before rendering. File collapse
  releases the loaded source; stale expansion asks for Refresh, not a silent
  replacement with newer or unrelated code.

The renderer is bundled offline with a nonpersistent WKWebView, one worker,
virtualized rows and a restricted native bridge. No CDN/HTTP server is involved.
See [renderer implementation](../apps/ios/DiffRenderer/README.md) for pinned
dependencies, resource rebuild steps, security and performance budgets.

All endpoints use the existing authenticated device relay. File content crosses
the relay over TLS; this is not an end-to-end encryption or on-phone-only claim.
The browser does not persist file/diff contents into chat history or a disk cache.

## Version and validation boundary

File browsing requires a remote engine containing the new RPCs and the updated
iOS client. Old engines report an upgrade notice for Files; Changes uses the
existing `GetCheckoutDiff` and `GetCheckoutFileDiffText` for context. A host
without the latter keeps its visible diff and reports Context unavailable.
No new Worker route, database migration, or remote
terminal command is needed.

The implementation is verified using isolated filesystem/RPC tests, a loopback
relay connecting two test engines with different files, and an offline iOS
Demo (`-demo -route chat:chat-tabs -sheet files`). Production engine processes, ongoing chats and installed
applications must not be restarted just to exercise the new handlers. A
real-device/remote smoke test is still required after an authorized engine and
iOS update. Use two different hosts/worktrees to verify target isolation.
