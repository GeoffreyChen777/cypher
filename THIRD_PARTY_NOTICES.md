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

The iOS app typesets math with SwiftMath, pinned in
`apps/ios/Cypher.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved`.

| Component | Version | License | Source |
| --- | --- | --- | --- |
| SwiftMath | 1.7.3 | MIT | https://github.com/mgriebling/SwiftMath |
| Latin Modern Math and TeX Gyre Termes Math fonts (bundled by SwiftMath) | 1.7.3 | GUST Font License | https://github.com/mgriebling/SwiftMath |
| XITS, Asana, Euler, Fira, Garamond, KpMath, Lete Sans, Libertinus and Noto Sans Math fonts (bundled by SwiftMath) | 1.7.3 | SIL Open Font License 1.1 | https://github.com/mgriebling/SwiftMath |

The full Cypher distribution remains licensed under the terms in `LICENSE`.
