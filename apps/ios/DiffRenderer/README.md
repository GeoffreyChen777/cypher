# iOS diff renderer

Pinned **@pierre/diffs 1.4.1**, vanilla JS (no React). The native session menu
opens separate Files and Changes sheets. Files remain SwiftUI/UIKit; Changes
uses one local WKWebView with a continuous, collapsible list of changed files.

## Build

```sh
cd apps/ios/DiffRenderer
npm ci --ignore-scripts
npm test
npm run build
```

Commit the source, `package-lock.json`, and generated
`../Cypher/Resources/DiffRenderer.bundle` together. Xcode copies the resource
bundle as-is; no npm, CDN, remote page, or local HTTP server is used at runtime.
`build.json` records the lockfile and script hashes. The bundle carries
third-party license/notice files (Pierre is Apache-2.0).

## Rendering policy

- Unified by default; the native Diff options menu switches to side-by-side.
- App-owned light/dark token themes, a compact change summary, stable 60px
  file headers and fixed-center SVG disclosures. Long filenames retain their
  extensions; counts use aligned tabular columns. Context/loading/error
  notices share the same icon and action layout. These styles are static
  (`src/presentation.mjs`), not derived from repository HTML or CSS.
- `VirtualizedFileDiff` / `Virtualizer`, with a 400px overscan window. Pure
  renames/metadata-only changes can reveal unchanged code using `VirtualizedFile`.
- One Blob worker per active viewer, **not** Pierre's default pool of eight.
  AST caches are limited to two entries per cache. Replacing the snapshot or
  changing layout/theme disposes all views, the virtualizer and worker before rebuilding.
- At most 250 file cards / 3 MiB patch text per document; 512 KiB / 10,000
  patch lines per file. Don't truncate a patch into a plausible
  but invalid structured diff. Raw patch fallback remains byte-bounded.
- Beyond 4,000 lines, or with a line longer than 2,000 characters, use plain
  diff coloring without syntax/word-level highlighting.
- Otherwise tokenization is capped at 1,000 characters per line, word comparison
  at 500. An explicit offline language allowlist replaces Shiki's full registry.
  Unknown file extensions use plain text.
- Unchanged regions start folded. Only an explicit expansion requests the
  before/after pair through the existing `GetCheckoutFileDiffText` RPC, pinned
  to the captured checkout/checksum. Native rejects stale, binary, truncated,
  over-512-KiB pairs or over-10,000-line sides. JS additionally verifies every
  hunk and unchanged gap before hydration. Stale data asks for Refresh without
  replacing the visible diff.
- At most two native context requests and 4 MiB retained source pairs per
  document. Collapsing a file releases its view/source; reopening starts folded.
  Context over 128 KiB or with long lines uses plain syntax coloring.
- Worker initialization and outstanding post-mount highlighting each have an 8s
  deadline; the native loading state has a 15s
  watchdog. Errors, process termination and limits offer Retry / Raw patch.

## Security and lifecycle

- `WKWebsiteDataStore.nonPersistent()`. The context bridge accepts only a
  native-owned file index in the current snapshot; JS cannot supply a path,
  checkout, device, RPC name or arbitrary filesystem/network operation.
  No shell or clipboard bridge. Copy is a native, explicit user action.
- Source text is passed as `callAsyncJavaScript` **arguments**, never interpolated
  into HTML, script source, CSS, or a URL.
- CSP blocks network connections, images, fonts, frames and forms. Only bundled
  scripts/styles and the locally generated worker are allowed. There is no
  `unsafe-eval`; `unsafe-inline` is allowed for styles only (renderer themes).
- Navigation is allowlisted to the bundled entry HTML, including new-window
  attempts. Renderer messages must come from that main frame and match the
  current request ID. Old callbacks and dismissed contexts cannot update UI.
- Teardown removes the bridge and navigates to an empty document to discard the
  worker, ASTs and repository content. No source is logged or persisted.

## Validation

Node policy tests plus `WorkspaceDiffWebTests` run the bundled renderer inside
real WKWebView on the iOS Simulator: worker initialization, styled/tokenized
output, unified/split and theme changes, viewport-bounded DOM, replacing old
content, literal HTML/script-like source, CSP violation and navigation denial,
oversized patch rejection, continuous file cards, explicit context loading,
unknown-index denial, stale expansion, release on collapse and pure renames.
UI tests cover separate menu destinations and tapping a real context expander.
Presentation regressions also assert icon/header geometry, aligned counts,
zero collapsed padding and long-path bounds. Demo-only screenshot attachments
cover light/dark, unified/split, collapsed, expanded context, menus, loading,
stale and binary/unavailable states.

These are **not physical-iPhone FPS/memory measurements**. Check first open,
long scrolling, repeated file changes and background/foreground on representative
devices before release. Budget all of WebContent and Worker memory, not just the
native process. Virtualization bounds DOM work, not total parsing/tokenization.
