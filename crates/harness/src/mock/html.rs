//! HTML preview demo reply for the mock harness: a tour of the demo
//! project's pages written as the file links agents use — Markdown links,
//! bare paths, a `:line` suffix, a folder — so clicking them opens each page
//! in the Files tab's HTML preview without a real model.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the HTML pages tour.
pub const MARKER: &str = "[mock:html]";

/// One reply section per entry; each streams as its own delta.
const SECTIONS: &[&str] = &[
    r##"I went through the demo site. Each link below opens in the Files tab — HTML pages open in **Preview**, the rest in the editor.

"##,
    r##"**Start here:** [index.html](index.html) links every other page and pulls in [style.css](style.css) by relative path, so a cream background means relative assets resolve.

"##,
    r##"| Page | What to look at |
| --- | --- |
| [Typography](pages/typography.html) | Tables, a wide code block that scrolls sideways, `<details>` |
| [Media](pages/media.html) | The relative SVG and PNG, and the deliberately missing image |
| [Interactive](pages/interactive.html) | Type into the field, then click back into Cypher — typing should follow |
| [Layout](pages/layout.html) | Resize the dock: 4 → 2 → 1 columns, and the live width readout |
| [Links](pages/links.html) | Every link kind with its expected behaviour |
| [Unicode](pages/unicode.html) | CJK, right-to-left text, emoji |

"##,
    r##"A few edge cases worth a click:

- `pages/legacy.htm` and `pages/document.xhtml` — the other two extensions also open in Preview.
- `pages/broken.html` — malformed markup; the page should still render.
- `pages/empty.html` — a zero-byte file.
- `pages/large.html` — over 256 KiB, so only the first 256 KiB load, with a read-only notice.
- [pages/interactive.html:53](pages/interactive.html:53) — a link with a line opens the **source** at that line (the page's script) instead.
- [pages/](pages/) — a folder link expands it in the tree.

"##,
    r##"The checklist is in [README.md](README.md) (Markdown preview). Links to the web, like [example.com](https://example.com), still open in your browser."##,
];

/// The showcase reply's text deltas.
pub(super) fn reply() -> Vec<AgentEvent> {
    SECTIONS
        .iter()
        .map(|section| AgentEvent::TextDelta {
            text: (*section).into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_point_into_the_demo_project_or_the_web() {
        let text: String = super::SECTIONS.concat();
        let targets: Vec<&str> = text
            .split("](")
            .skip(1)
            .filter_map(|rest| rest.split_once(')').map(|(target, _)| target))
            .collect();
        assert!(targets.len() >= 10, "{targets:?}");
        for target in targets {
            assert!(
                target.starts_with("https://") || !target.contains(':') || target.ends_with(":53"),
                "{target}"
            );
            assert!(!target.starts_with('/'), "{target}");
        }
    }
}
