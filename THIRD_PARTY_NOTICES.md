# Third-party notices

Cypher bundles the following syntax-highlighting components. Their parsers and
queries are consumed from the pinned Rust crates listed in `Cargo.lock`.

| Component | Version | License | Source |
| --- | --- | --- | --- |
| Tree-sitter | 0.26.11 | MIT | https://github.com/tree-sitter/tree-sitter |
| Tree-sitter highlight | 0.26.11 | MIT | https://github.com/tree-sitter/tree-sitter |
| Tree-sitter Rust grammar and queries | 0.24.2 | MIT | https://github.com/tree-sitter/tree-sitter-rust |
| Tree-sitter JavaScript grammar and queries | 0.25.0 | MIT | https://github.com/tree-sitter/tree-sitter-javascript |
| Tree-sitter TypeScript grammar and queries | 0.23.2 | MIT | https://github.com/tree-sitter/tree-sitter-typescript |
| Tree-sitter Python, Go, JSON, Bash, HTML, CSS, C, C++, C#, Java, Ruby and PHP grammars and queries | pinned in `Cargo.lock` | MIT | https://github.com/tree-sitter |
| Tree-sitter TOML, Markdown, YAML, Kotlin, Swift, SQL, Lua, Nix, Make and Containerfile grammars and queries | pinned in `Cargo.lock` | MIT-compatible; see each crate | Crate repositories recorded in `Cargo.lock` |

Math in chat markdown is typeset with the following components.

| Component | Version | License | Source |
| --- | --- | --- | --- |
| RaTeX (parser, layout, raster renderer) | 0.1.15 | MIT | https://github.com/erweixin/RaTeX |
| KaTeX fonts (`KaTeX_*.ttf`, embedded via `ratex-katex-fonts`) | 0.1.15 | SIL Open Font License 1.1 | https://github.com/KaTeX/KaTeX |

Mermaid diagrams in chat markdown are laid out with Merman, built without its
EPL-2.0 ELK layout (`default-features = false`; `all-diagrams`, `svg`,
`layout-cytoscape`). Merman records the sources it translates or embeds in
its own `THIRD_PARTY_NOTICES.md`; the ones this build carries are below.

| Component | Version | License | Source |
| --- | --- | --- | --- |
| Merman (`merman`, `merman-core`, `merman-render` and their layout crates) | 0.9.2 | MIT OR Apache-2.0 | https://github.com/Latias94/merman |
| Mermaid (algorithms, defaults and icon data, translated or copied by Merman) | 12.1.0 | MIT | https://github.com/mermaid-js/mermaid |
| d3-shape (curve algorithms, translated by Merman) | 3.2.0 | ISC | https://github.com/d3/d3-shape |
| rough-rs roughr and Rough.js (hand-drawn look, forked and translated by Merman) | 0.12.0 / 4.6.6 | MIT | https://github.com/orhanbalci/rough-rs |
| @upsetjs/venn.js (Venn layout, translated by Merman) | 2.0.0 | MIT | https://github.com/upsetjs/venn.js |
| fmin (optimizers, translated by Merman) | 0.0.4 | BSD-3-Clause | https://github.com/benfred/fmin |
| unicode-linebreak (forked by Merman) | 0.1.5 | Apache-2.0 | https://github.com/axelf4/unicode-linebreak |
| ZenUML Core (grammar and renderer behavior, followed by Merman) | 3.50.1 | MIT | https://github.com/mermaid-js/zenuml-core |
| cssparser, selectors and dtoa-short (Merman's CSS handling, used unmodified) | pinned in `Cargo.lock` | MPL-2.0 | https://github.com/servo/rust-cssparser, https://github.com/servo/stylo |

The iOS app draws Mermaid diagrams with mermaid.js, bundled offline in
`apps/ios/Cypher/Resources/MermaidRenderer.bundle` without its EPL-2.0 ELK
layout (see `apps/ios/MermaidRenderer/README.md`). The bundle's
`THIRD_PARTY_NOTICES.txt` carries the license and notice files of every
package it includes.

| Component | Version | License | Source |
| --- | --- | --- | --- |
| Mermaid | 12.2.0 | MIT | https://github.com/mermaid-js/mermaid |
| Mermaid's dependencies (d3, dagre-d3-es, cytoscape and its layouts, khroma, marked, KaTeX, roughjs, chevrotain, lodash-es, dayjs and others) | pinned in `apps/ios/MermaidRenderer/package-lock.json` | MIT, ISC, Apache-2.0, BSD-3-Clause or Unlicense | Package repositories recorded in the lockfile |
| DOMPurify (Mermaid's label sanitizer) | pinned in the lockfile | MPL-2.0 OR Apache-2.0 (used under Apache-2.0) | https://github.com/cure53/DOMPurify |

The iOS app typesets math with SwiftMath, pinned in
`apps/ios/Cypher.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved`.

| Component | Version | License | Source |
| --- | --- | --- | --- |
| SwiftMath | 1.7.3 | MIT | https://github.com/mgriebling/SwiftMath |
| Latin Modern Math and TeX Gyre Termes Math fonts (bundled by SwiftMath) | 1.7.3 | GUST Font License | https://github.com/mgriebling/SwiftMath |
| XITS, Asana, Euler, Fira, Garamond, KpMath, Lete Sans, Libertinus and Noto Sans Math fonts (bundled by SwiftMath) | 1.7.3 | SIL Open Font License 1.1 | https://github.com/mgriebling/SwiftMath |

The full Cypher distribution remains licensed under the terms in `LICENSE`.
