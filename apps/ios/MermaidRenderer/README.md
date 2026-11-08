# iOS Mermaid renderer

Pinned **mermaid 12.2.0**, bundled offline without ELK. It draws
```` ```mermaid ```` fences in the transcript; the desktop draws the same fences
natively with Merman (`crates/ui/src/markdown/mermaid.rs`).

## Build

```sh
cd apps/ios/MermaidRenderer
npm ci --ignore-scripts
npm test
npm run build
```

Commit the source, `package-lock.json`, and generated
`../Cypher/Resources/MermaidRenderer.bundle` together. Xcode copies the
resource bundle as-is; no npm, CDN, remote page, or local HTTP server is used
at runtime. `build.json` records the lockfile and script hashes, and the bundle
carries the bundled packages' license/notice files.

## Without ELK

Mermaid 12 depends on `elkjs` (EPL-2.0) and makes ELK the default layout for
several families. The build removes Mermaid's ELK layout loaders (an anchor
that must match exactly once, so a Mermaid update that moves it fails the
build) and stubs `elkjs`; diagrams that ask for ELK take Mermaid's own fallback
to dagre. The build also fails if ELK code reaches the bundle.

## Rendering

- `MermaidRenderer.swift` owns one hidden, non-persistent WKWebView for every
  diagram, renders one at a time (Mermaid's configuration is global), and
  closes it after a minute idle. Settled diagrams are cached (48, least
  recently used first), keyed by source, palette and display scale.
- The page lays the diagram out (so labels are measured in the font they are
  drawn in), draws the SVG onto a canvas filled with the code panel's color,
  and returns a PNG with its natural size. Labels are SVG text
  (`htmlLabels: false`), which a canvas can draw. Rasters are capped at
  4096 px a side.
- The palette is the chat theme's: the code-block fill is the canvas, nodes
  are the ink washes chat chrome uses, the series colors are the accents
  (desktop `Palette::of`). The policy module validates every color as
  `#rrggbb` before it reaches Mermaid.
- Errors come back as one bounded line ("Parse error on line 3: unexpected
  end of input"); the fence stays code with that reason in its header.

## Security and lifecycle

- `WKWebsiteDataStore.nonPersistent()`; no script message handler — the page
  cannot call into the app. Source text and palette cross as
  `callAsyncJavaScript` **arguments**, never interpolated into HTML or script.
- `securityLevel: "strict"`: labels are text, with no links, click handlers or
  HTML.
- CSP: `default-src 'none'`, bundled script only, inline styles (Mermaid's
  SVG styles), `img-src blob:` for the SVG it rasterizes; no network, fonts,
  frames or forms. Navigation is allowlisted to the bundled entry HTML.
- A render past its deadline (15s, covering a cold WebContent launch) or a
  WebContent crash closes the web view; the next render starts a fresh one.

## Validation

`npm test` covers the policy module (palette validation, theme variables,
error lines, raster caps). `MermaidTests` runs the bundled renderer in a real
WKWebView on the iOS Simulator: common families, CJK labels, parse errors and
recovery, the ELK fallback, markup in labels, the cache, and the parser's
closed-fence rule. `MermaidUITests` switches a demo diagram to its source and
back by real taps in both appearances (`-demo -route chat:chat-mermaid`).
