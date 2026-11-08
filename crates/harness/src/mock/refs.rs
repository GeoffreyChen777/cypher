//! File-reference demo reply for the mock harness: an answer that cites
//! code the way agents do — full paths with lines, paths relative to a
//! crate, a bare name after its full path, an ambiguous name, a missing
//! file — beside inline code that must stay code. The paths are this
//! repository's, so in a chat opened on a Cypher checkout the chips preview
//! and open real files.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the file-reference showcase.
pub const MARKER: &str = "[mock:refs]";

const REPLY: &str = r##"**How a reference becomes a chip** — the renderer flattens each paragraph's runs (`crates/ui/src/markdown/render.rs:1032-1045`) and asks `detect()` whether an inline code span is a file reference (`crates/ui/src/markdown/code_ref.rs:51-83`). Labels are decided across the paragraph (`render.rs:943-964`, a bare name read as the full path above), so two different `mod.rs` files would keep a folder each.

Hovering is tracked by an invisible layer over the chips (`render.rs:1538-1598`); the card itself lives in the transcript (`src/transcript/code_ref.rs:190-214`, written relative to the crate). Clicking goes through the same file links as Markdown links (`files/links.rs:23-59`), which now carry a line range.

What stays plain code: identifiers like `identity()` and `world_size`, placeholders such as `data/rank_<n>.pt`, globs like `src/**/*.rs`, and bare names without lines like `Cargo.toml`.

Edge cases to hover:

- an ambiguous path — `src/lib.rs` exists in several crates;
- a file that is not in this project — `core/context.py:146-160`;
- a whole file without lines — `docs/features/files.md`.
"##;

/// Bytes per streamed delta: chips arrive half-written while streaming.
const CHUNK: usize = 24;

/// The showcase reply's text deltas.
pub(super) fn reply() -> Vec<AgentEvent> {
    let mut deltas = Vec::new();
    let mut start = 0;
    while start < REPLY.len() {
        let mut end = (start + CHUNK).min(REPLY.len());
        while !REPLY.is_char_boundary(end) {
            end += 1;
        }
        deltas.push(AgentEvent::TextDelta {
            text: REPLY[start..end].into(),
        });
        start = end;
    }
    deltas
}
