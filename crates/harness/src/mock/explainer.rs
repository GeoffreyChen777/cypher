//! Session explainer demo replies for the mock harness: the `/explain` page,
//! then the patches a live explainer writes as the work goes on: one whose
//! edits all apply, one with an edit that matches nothing, and one to a page
//! the chat never had. So the patch cards' states and an open page reloading
//! in place can be seen without a real model.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the explainer page.
pub const MARKER: &str = "[mock:explainer]";
/// Prompt marker: reply with a patch whose edits all apply.
pub const PATCH_MARKER: &str = "[mock:explainer:patch]";
/// Prompt marker: reply with a patch one of whose edits matches nothing.
pub const PARTIAL_MARKER: &str = "[mock:explainer:patch:partial]";
/// Prompt marker: reply with a patch to a page the chat doesn't have.
pub const ORPHAN_MARKER: &str = "[mock:explainer:patch:orphan]";

const TITLE: &str = "Session explainer";

const PAGE: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Session explainer</title>
<style>
  body { font: 15px/1.5 -apple-system, system-ui, sans-serif; margin: 24px; color: #1d1d1f; }
  h1 { font-size: 20px; margin: 0 0 4px; }
  h2 { font-size: 13px; margin: 20px 0 6px; color: #6e6e73; text-transform: uppercase; letter-spacing: .04em; }
  li { margin: 4px 0; }
  .done { color: #1a7f37; }
</style>
</head>
<body>
<h1>Live session explainer</h1>
<p>What this session built, kept up to date as the chat goes on.</p>
<h2>What changed</h2>
<ul>
  <li>Patch fences: <code>artifact-patch</code> edits a page in place.</li>
</ul>
<h2>How it fits</h2>
<p>The chat folds every revision of a page in order; the newest one opens.</p>
<h2>Still open</h2>
<ul>
  <li>The iPhone app shows patches as code.</li>
  <li>Side chats don't follow revisions.</li>
</ul>
</body>
</html>"#;

/// Every edit applies: a new item, and an open one marked done.
const PATCH: &[(&str, &str)] = &[
    (
        "  <li>Patch fences: <code>artifact-patch</code> edits a page in place.</li>",
        "  <li>Patch fences: <code>artifact-patch</code> edits a page in place.</li>\n  <li>The iPhone app applies patches too, reading the whole chat.</li>",
    ),
    (
        "  <li>The iPhone app shows patches as code.</li>",
        "  <li class=\"done\">The iPhone app shows patches as code. Fixed.</li>",
    ),
];

/// The second edit's SEARCH is nowhere on the page: 2 of 3 apply.
const PARTIAL: &[(&str, &str)] = &[
    (
        "<p>The chat folds every revision of a page in order; the newest one opens.</p>",
        "<p>The chat folds every revision of a page in order; the newest one opens, and an open page reloads.</p>",
    ),
    (
        "  <li>Mock sessions for every patch outcome.</li>",
        "  <li class=\"done\">Mock sessions for every patch outcome.</li>",
    ),
    (
        "  <li>Side chats don't follow revisions.</li>",
        "  <li>Side chats don't follow revisions yet.</li>",
    ),
];

/// A page under another title, which no earlier reply wrote.
const ORPHAN: &[(&str, &str)] = &[("<h1>Release notes</h1>", "<h1>Release notes 1.2</h1>")];

/// The explainer page, as `/explain` asks for it.
pub(super) fn reply() -> Vec<AgentEvent> {
    let fence = format!("```html artifact title=\"{TITLE}\"\n{PAGE}\n```\n");
    let pieces = fence.split_inclusive("</style>\n").map(str::to_string);
    deltas(
        std::iter::once("Here's the session so far:\n\n".to_string())
            .chain(pieces)
            .chain(std::iter::once(
                "\nIt updates as we go; open it beside the chat.\n".to_string(),
            )),
    )
}

/// The patch whose edits all apply.
pub(super) fn patch() -> Vec<AgentEvent> {
    patch_reply(
        "Ported the patches to the iPhone app.",
        TITLE,
        PATCH,
        "The explainer has the new item and the open one closed.",
    )
}

/// The patch with an edit that matches nothing.
pub(super) fn partial() -> Vec<AgentEvent> {
    patch_reply(
        "Open pages now reload when a revision lands.",
        TITLE,
        PARTIAL,
        "One edit looked for a line the page never had, so it was skipped.",
    )
}

/// The patch to a page the chat doesn't have.
pub(super) fn orphan() -> Vec<AgentEvent> {
    patch_reply(
        "Bumped the release notes.",
        "Release notes",
        ORPHAN,
        "There was no release notes page to update, so nothing changed.",
    )
}

fn patch_reply(lead: &str, title: &str, hunks: &[(&str, &str)], after: &str) -> Vec<AgentEvent> {
    let fence = patch_fence(title, hunks);
    let pieces = fence
        .split_inclusive(">>>>>>> REPLACE\n")
        .map(str::to_string);
    deltas(
        std::iter::once(format!("{lead}\n\n"))
            .chain(pieces)
            .chain(std::iter::once(format!("\n{after}\n"))),
    )
}

fn patch_fence(title: &str, hunks: &[(&str, &str)]) -> String {
    let hunks: String = hunks
        .iter()
        .map(|(from, to)| format!("<<<<<<< SEARCH\n{from}\n=======\n{to}\n>>>>>>> REPLACE\n"))
        .collect();
    format!("```html artifact-patch title=\"{title}\"\n{hunks}```\n")
}

fn deltas(texts: impl Iterator<Item = String>) -> Vec<AgentEvent> {
    texts.map(|text| AgentEvent::TextDelta { text }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies hunks the way the chat does when each matches verbatim:
    /// replacing it once. Returns how many matched exactly one place.
    fn apply(page: &mut String, hunks: &[(&str, &str)]) -> usize {
        let mut applied = 0;
        for (from, to) in hunks {
            if page.matches(from).count() == 1 {
                *page = page.replacen(from, to, 1);
                applied += 1;
            }
        }
        applied
    }

    #[test]
    fn the_patches_apply_as_their_cards_say() {
        let mut page = PAGE.to_string();
        assert_eq!(apply(&mut page, PATCH), 2);
        assert!(page.contains("reading the whole chat"));
        assert_eq!(apply(&mut page, PARTIAL), 2, "the second edit misses");
        assert!(page.contains("revisions yet."));
        assert!(!PAGE.contains("Release notes"));
    }

    #[test]
    fn every_reply_closes_its_fences_and_streams_in_pieces() {
        for reply in [reply(), patch(), partial(), orphan()] {
            let text: String = reply
                .iter()
                .map(|event| match event {
                    AgentEvent::TextDelta { text } => text.as_str(),
                    _ => "",
                })
                .collect();
            assert_eq!(text.matches("```").count(), 2, "{text}");
            assert!(reply.len() >= 3);
        }
        let first: String = patch_fence(TITLE, PATCH);
        assert!(first.starts_with("```html artifact-patch title=\"Session explainer\"\n"));
    }
}
