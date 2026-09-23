//! Map a transcript quote back to the words the agent has.
//!
//! With Pi translation on, the transcript shows a TRANSLATION on one side of
//! the conversation: an answer is displayed in the user's language while the
//! agent wrote it in another, and a prompt is displayed as typed while the
//! agent read its translation. A quote selected from that display is text the
//! agent never saw. Each translated text part keeps the agent's version
//! (`agent_text`), and this module finds the original PASSAGE a settled
//! selection came from.
//!
//! The passage is found positionally, never by a text search: the
//! translation prompt preserves Markdown structure, so a translated answer
//! and its original parse into the same top-level blocks. A selection
//! resolves (through its element keys) to the blocks it covers, and the
//! passage is the same blocks of the original — list items when it sits
//! inside one list, paragraphs of a user's prompt. Wherever the two versions
//! do not line up block for block the passage is the whole original: a wider
//! passage is honest, a misaligned one would put words in the agent's mouth.
//!
//! The exact original WORDS are found later, by the translation extension,
//! from the passage and the displayed text around the selection
//! ([`QuoteAlign`]); the agent is never shown the translation itself.

use std::ops::Range;

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_proto::agent_prompt::{AgentQuote, QuoteAlign};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::markdown::parser::{self, Block, BlockTree, parse_full};
use crate::markdown::selection::{Span, row_of_key};

/// What append mode puts between an answer and its translation
/// (`renderTranslation` in `dist/pi-runtime/extensions/cypher-translation.ts`).
const APPEND_SEPARATOR: &str = "\n\n---\n\n";

/// What a settled transcript selection stands for in the agent's words, or
/// `None` when no part of it was translated — the quote is then the agent's
/// own text already and must be used exactly as selected.
///
/// A selection inside one translated passage yields [`AgentQuote::Align`];
/// one spanning several messages yields the original passages it covers,
/// with untranslated parts as selected ([`AgentQuote::Passage`]).
///
/// `spans` are the selection's spans in document order
/// ([`SelectionSnapshot::spans`](crate::markdown::selection::SelectionSnapshot));
/// `entries` the continuation-joined transcript the rows were built from.
pub fn agent_quote(entries: &[SessionMessageEntry], spans: &[Span]) -> Option<AgentQuote> {
    let mut units: Vec<(Unit, Vec<(&Span, usize)>)> = Vec::new();
    for span in spans {
        if span.range.is_empty() && !span.text.is_empty() {
            continue;
        }
        let (unit, block_ix) = locate(entries, &span.key);
        match units.last_mut() {
            Some((last, members)) if *last == unit && unit != Unit::Other => {
                members.push((span, block_ix));
            }
            _ => units.push((unit, vec![(span, block_ix)])),
        }
    }
    let mut aligns: Vec<Option<QuoteAlign>> = Vec::new();
    for (unit, members) in &units {
        aligns.push(match *unit {
            Unit::Part { entry, part } => match &entries[entry].parts[part] {
                MessagePart::Text {
                    text,
                    agent_text: Some(agent_text),
                    ..
                } => map_answer(text, agent_text, members)
                    .map(|passage| shown_in_elements(passage, members)),
                _ => None,
            },
            Unit::User { entry } => match &entries[entry].parts.first() {
                Some(MessagePart::Text {
                    agent_text: Some(agent_text),
                    ..
                }) => map_prompt(agent_text, members),
                _ => None,
            },
            Unit::Other => None,
        });
    }
    if aligns.iter().all(Option::is_none) {
        return None;
    }
    if let [Some(align)] = aligns.as_slice() {
        return Some(AgentQuote::Align(align.clone()));
    }
    let text = units
        .iter()
        .zip(&aligns)
        .map(|((_, members), align)| match align {
            Some(align) => align.passage.clone(),
            None => verbatim(members),
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(AgentQuote::Passage { text })
}

/// Where a run of spans sits: one text part of an answer, one user prompt, or
/// anything else (tool chips, rows a commit replaced) — taken as selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Part { entry: usize, part: usize },
    User { entry: usize },
    Other,
}

/// Resolve an element key to its unit and top-level block. Answer rows are
/// keyed `{entry}#{part}.{block}` (`rows_for_entry`) with the renderer's
/// `-t{element}` suffix; a user bubble is `{entry}:u`.
fn locate(entries: &[SessionMessageEntry], key: &str) -> (Unit, usize) {
    if let Some(row) = key.strip_suffix(":u") {
        if let Some(entry) = entries
            .iter()
            .position(|e| e.id == row && e.role == MessageRole::User)
        {
            return (Unit::User { entry }, 0);
        }
        return (Unit::Other, 0);
    }
    let row = row_of_key(key);
    for (entry, e) in entries.iter().enumerate() {
        let Some(rest) = row
            .strip_prefix(e.id.as_str())
            .and_then(|rest| rest.strip_prefix('#'))
        else {
            continue;
        };
        let Some((part_id, block)) = rest.rsplit_once('.') else {
            continue;
        };
        let Ok(block_ix) = block.parse::<usize>() else {
            continue;
        };
        if let Some(part) = e
            .parts
            .iter()
            .position(|p| matches!(p, MessagePart::Text { id, .. } if id == part_id))
        {
            return (Unit::Part { entry, part }, block_ix);
        }
    }
    (Unit::Other, 0)
}

/// The selected text itself — the copy/quote join
/// ([`selection::selected_text`](crate::markdown::selection::selected_text)).
fn verbatim(members: &[(&Span, usize)]) -> String {
    members
        .iter()
        .map(|(span, _)| &span.text[span.range.clone()])
        .collect::<Vec<_>>()
        .join("\n")
}

/// The displayed text around a selection in an answer: the selected
/// elements' text before and after the selection. Elements are paragraphs,
/// list items, headings — the translated passage the selection sits in.
fn shown_in_elements(passage: String, members: &[(&Span, usize)]) -> QuoteAlign {
    let first = members.first().map(|(span, _)| *span);
    let last = members.last().map(|(span, _)| *span);
    QuoteAlign {
        passage,
        before: first.map_or_else(String::new, |span| span.text[..span.range.start].to_owned()),
        selected: verbatim(members),
        after: last.map_or_else(String::new, |span| span.text[span.range.end..].to_owned()),
    }
}

/// One answer's selected blocks, in the words the model wrote. `None` when
/// the selection sits in the original half of an append-mode rendering: that
/// text IS the original, so the exact selection stands.
fn map_answer(display: &str, agent: &str, members: &[(&Span, usize)]) -> Option<String> {
    if let Some(translation) = display
        .strip_prefix(agent)
        .and_then(|rest| rest.strip_prefix(APPEND_SEPARATOR))
    {
        // Display blocks: the original's, the separator rule, the translation's.
        let original_blocks = parse_full(agent).len();
        if members.iter().all(|(_, block)| *block < original_blocks) {
            return None;
        }
        if members.iter().all(|(_, block)| *block > original_blocks) {
            let shifted: Vec<(&Span, usize)> = members
                .iter()
                .map(|(span, block)| (*span, block - original_blocks - 1))
                .collect();
            return Some(map_blocks(translation, agent, &shifted));
        }
        return Some(agent.trim().to_string());
    }
    Some(map_blocks(display, agent, members))
}

/// Map selected blocks of `shown` onto the same blocks of `agent`.
fn map_blocks(shown: &str, agent: &str, members: &[(&Span, usize)]) -> String {
    let whole = || agent.trim().to_string();
    let shown_tree = parse_full(shown);
    let agent_tree = parse_full(agent);
    if !aligned(&shown_tree, &agent_tree) {
        return whole();
    }
    let (Some(lo), Some(hi)) = (
        members.iter().map(|(_, block)| *block).min(),
        members.iter().map(|(_, block)| *block).max(),
    ) else {
        return whole();
    };
    if hi >= agent_tree.len() {
        return whole();
    }
    // Inside one list the item is the unit, not the whole list.
    if lo == hi
        && let Block::List { items, .. } = &shown_tree.blocks[lo].block
        && let Some(picked) = selected_items(items, members)
    {
        let ranges = list_item_ranges(agent, agent_tree.blocks[lo].range.clone());
        if ranges.len() == items.len() {
            return agent[ranges[picked.start].start..ranges[picked.end - 1].end]
                .trim()
                .to_string();
        }
    }
    agent[agent_tree.blocks[lo].range.start..agent_tree.blocks[hi].range.end]
        .trim()
        .to_string()
}

/// Whether two parses line up block for block: same count, same kind at every
/// position, lists with the same number of items, and code blocks with the
/// same code (translation must leave code alone — a code block that changed
/// means the structure cannot be trusted).
fn aligned(shown: &BlockTree, agent: &BlockTree) -> bool {
    shown.len() == agent.len()
        && shown
            .blocks
            .iter()
            .zip(&agent.blocks)
            .all(|(s, a)| match (&s.block, &a.block) {
                (Block::CodeBlock { code: sc, .. }, Block::CodeBlock { code: ac, .. }) => sc == ac,
                (Block::List { items: si, .. }, Block::List { items: ai, .. }) => {
                    si.len() == ai.len()
                }
                (s, a) => std::mem::discriminant(s) == std::mem::discriminant(a),
            })
}

/// The top-level items a selection inside one list covers. Each selected
/// element is found by its text in exactly one item; any element that
/// matches no item, or several, leaves the whole list as the unit.
fn selected_items(items: &[Vec<Block>], members: &[(&Span, usize)]) -> Option<Range<usize>> {
    let texts: Vec<String> = items.iter().map(|item| squash(&plain(item))).collect();
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for (span, _) in members {
        let needle = squash(&span.text);
        if needle.is_empty() {
            continue;
        }
        let mut hits = texts
            .iter()
            .enumerate()
            .filter(|(_, text)| text.contains(&needle));
        let (ix, _) = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        lo = lo.min(ix);
        hi = hi.max(ix);
    }
    (lo <= hi).then_some(lo..hi + 1)
}

/// A block sequence's text as rendered (runs concatenated, blocks on lines).
fn plain(blocks: &[Block]) -> String {
    let mut out: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            Block::Paragraph { runs } | Block::Heading { runs, .. } => {
                out.push(runs.iter().map(|run| run.text.as_str()).collect());
            }
            Block::CodeBlock { code, .. } => out.push(code.clone()),
            Block::BlockQuote { children } => out.push(plain(children)),
            Block::List { items, .. } => {
                for item in items {
                    out.push(plain(item));
                }
            }
            Block::Table { header, rows, .. } => {
                for row in std::iter::once(header).chain(rows) {
                    for cell in row {
                        out.push(cell.iter().map(|run| run.text.as_str()).collect());
                    }
                }
            }
            Block::Rule => {}
        }
    }
    out.join("\n")
}

/// Text with every whitespace character removed — the renderer inserts thin
/// spaces around inline code and reflows soft breaks, so element text and
/// run text agree only once spacing is ignored.
fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Source ranges of the top-level items of the list occupying `range`.
fn list_item_ranges(source: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let Some(block) = source.get(range.clone()) else {
        return Vec::new();
    };
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (event, at) in Parser::new_ext(block, parser::options()).into_offset_iter() {
        match event {
            Event::Start(Tag::List(_)) => depth += 1,
            Event::End(TagEnd::List(_)) => depth = depth.saturating_sub(1),
            Event::Start(Tag::Item) if depth == 1 => {
                out.push(range.start + at.start..range.start + at.end);
            }
            _ => {}
        }
    }
    out
}

/// A prompt's selected paragraphs, in the words the agent read, with the
/// displayed paragraphs around the selection. A user bubble renders its text
/// as typed (one element; the span's text is the displayed prompt), so
/// paragraphs are the blank-line-separated runs of it.
fn map_prompt(agent: &str, members: &[(&Span, usize)]) -> Option<QuoteAlign> {
    let agent = crate::attachments::parse_user_message_images(agent).text;
    let (first, _) = members.first()?;
    let (last, _) = members.last()?;
    let shown_text = first.text.as_str();
    let (start, end) = (first.range.start, last.range.end);
    let shown = paragraphs(shown_text);
    let original = paragraphs(&agent);
    let aligned = !shown.is_empty() && shown.len() == original.len();
    let covering = |at: usize| {
        shown
            .iter()
            .position(|p| at < p.end)
            .unwrap_or(shown.len().saturating_sub(1))
    };
    let (lo, hi) = (covering(start), covering(end.saturating_sub(1).max(start)));
    let (passage, from, to) = if aligned {
        (
            agent[original[lo].start..original[hi].end]
                .trim()
                .to_string(),
            shown[lo].start.min(start),
            shown[hi].end.max(end),
        )
    } else {
        (agent.trim().to_string(), 0, shown_text.len())
    };
    Some(QuoteAlign {
        passage,
        before: shown_text[from..start].to_owned(),
        selected: shown_text[start..end].to_owned(),
        after: shown_text[end..to].to_owned(),
    })
}

/// Byte ranges of the blank-line-separated paragraphs of `text`.
fn paragraphs(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if content.trim().is_empty() {
            if let Some(s) = start.take() {
                out.push(s..end);
            }
        } else {
            start.get_or_insert(at);
            end = at + content.len();
        }
        at += line.len();
    }
    if let Some(s) = start {
        out.push(s..end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_doc::MessageStatus;

    fn answer(id: &str, text: &str, agent_text: Option<&str>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: text.into(),
                agent_text: agent_text.map(Into::into),
            }],
            created_at: 0,
            device_id: "d".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            completed_at: None,
        }
    }

    fn prompt(id: &str, text: &str, agent_text: Option<&str>) -> SessionMessageEntry {
        SessionMessageEntry {
            role: MessageRole::User,
            ..answer(id, text, agent_text)
        }
    }

    /// A span over `selected` inside the element whose flat text is `element`,
    /// in block `block` of `entry`'s part `t0`.
    fn block_span(entry: &str, block: usize, element: &str, selected: &str) -> Span {
        let start = element.find(selected).expect("selected text in element");
        Span {
            key: format!("{entry}#t0.{block}-t0"),
            range: start..start + selected.len(),
            text: element.into(),
        }
    }

    fn bubble_span(entry: &str, shown: &str, selected: &str) -> Span {
        let start = shown.find(selected).expect("selected text in bubble");
        Span {
            key: format!("{entry}:u"),
            range: start..start + selected.len(),
            text: shown.into(),
        }
    }

    fn passage(quote: Option<AgentQuote>) -> String {
        quote.expect("translated").fallback().to_owned()
    }

    fn aligned(quote: Option<AgentQuote>) -> QuoteAlign {
        match quote {
            Some(AgentQuote::Align(align)) => align,
            other => panic!("expected an alignment, got {other:?}"),
        }
    }

    #[test]
    fn untranslated_text_is_left_exactly_as_selected() {
        let entries = [answer("a", "First.\n\nSecond.", None)];
        let spans = [block_span("a", 1, "Second.", "Sec")];
        assert_eq!(agent_quote(&entries, &spans), None);
    }

    #[test]
    fn a_translated_selection_carries_its_passage_and_the_text_around_it() {
        let entries = [answer(
            "a",
            "第一段。\n\n第二段，很长。\n\n第三段。",
            Some("First paragraph.\n\nSecond paragraph, long.\n\nThird paragraph."),
        )];
        let spans = [block_span("a", 1, "第二段，很长。", "很长")];
        assert_eq!(
            aligned(agent_quote(&entries, &spans)),
            QuoteAlign {
                passage: "Second paragraph, long.".into(),
                before: "第二段，".into(),
                selected: "很长".into(),
                after: "。".into(),
            }
        );
        // Across blocks: the passage runs from the first covered block to the
        // last, the displayed context from the first element to the last.
        let spans = [
            block_span("a", 1, "第二段，很长。", "很长。"),
            block_span("a", 2, "第三段。", "第三"),
        ];
        let align = aligned(agent_quote(&entries, &spans));
        assert_eq!(align.passage, "Second paragraph, long.\n\nThird paragraph.");
        assert_eq!(
            (
                align.before.as_str(),
                align.selected.as_str(),
                align.after.as_str()
            ),
            ("第二段，", "很长。\n第三", "段。")
        );
    }

    #[test]
    fn a_selection_inside_one_list_takes_its_items_as_the_passage() {
        let entries = [answer(
            "a",
            "要点：\n\n- 甲项\n- 乙项\n- 丙项",
            Some("Points:\n\n- item A\n- item B\n- item C"),
        )];
        let spans = [block_span("a", 1, "乙项", "乙")];
        assert_eq!(passage(agent_quote(&entries, &spans)), "- item B");
        let spans = [
            block_span("a", 1, "乙项", "项"),
            block_span("a", 1, "丙项", "丙"),
        ];
        assert_eq!(passage(agent_quote(&entries, &spans)), "- item B\n- item C");
        // Ambiguous element text keeps the whole list.
        let spans = [block_span("a", 1, "项", "项")];
        assert_eq!(
            passage(agent_quote(&entries, &spans)),
            "- item A\n- item B\n- item C"
        );
    }

    #[test]
    fn misaligned_structure_takes_the_whole_original_as_the_passage() {
        // Block counts differ.
        let entries = [answer("a", "一。\n\n二。", Some("One. Two."))];
        let spans = [block_span("a", 1, "二。", "二")];
        assert_eq!(passage(agent_quote(&entries, &spans)), "One. Two.");
        // Same shape, but the code changed under translation.
        let entries = [answer(
            "a",
            "说明。\n\n```sh\n# 构建\nmake\n```",
            Some("Note.\n\n```sh\n# build\nmake\n```"),
        )];
        let spans = [block_span("a", 0, "说明。", "说明")];
        assert_eq!(
            passage(agent_quote(&entries, &spans)),
            "Note.\n\n```sh\n# build\nmake\n```"
        );
    }

    #[test]
    fn append_mode_keeps_the_original_half_exact_and_aligns_the_translated_half() {
        let original = "First.\n\nSecond.";
        let display = format!("{original}{APPEND_SEPARATOR}第一。\n\n第二。");
        let entries = [answer("a", &display, Some(original))];
        // Blocks 0..2 are the original: already the agent's words.
        let spans = [block_span("a", 1, "Second.", "Sec")];
        assert_eq!(agent_quote(&entries, &spans), None);
        // Block 2 is the rule; 3..5 the translation.
        let spans = [block_span("a", 4, "第二。", "第二")];
        assert_eq!(passage(agent_quote(&entries, &spans)), "Second.");
        // Straddling both halves: the whole original.
        let spans = [
            block_span("a", 1, "Second.", "Second."),
            block_span("a", 3, "第一。", "第一"),
        ];
        assert_eq!(passage(agent_quote(&entries, &spans)), original);
    }

    #[test]
    fn a_prompt_aligns_by_paragraph_without_its_attachment_trailer() {
        let shown = "第一段\n\n第二段在这里";
        let entries = [prompt(
            "u",
            shown,
            Some(
                "First part\n\nSecond part here\n\nAttached images (local files — open them to view):\n- /a.png",
            ),
        )];
        let spans = [bubble_span("u", shown, "在这")];
        assert_eq!(
            aligned(agent_quote(&entries, &spans)),
            QuoteAlign {
                passage: "Second part here".into(),
                before: "第二段".into(),
                selected: "在这".into(),
                after: "里".into(),
            }
        );
    }

    /// A selection across messages cannot be aligned as one passage: each
    /// translated message contributes its original passage, the rest is
    /// taken as selected.
    #[test]
    fn a_selection_across_messages_quotes_whole_original_passages() {
        let entries = [
            prompt("u", "看这里", None),
            answer("a", "译文。", Some("Original.")),
        ];
        let spans = [
            bubble_span("u", "看这里", "这里"),
            block_span("a", 0, "译文。", "译文"),
        ];
        assert_eq!(
            agent_quote(&entries, &spans),
            Some(AgentQuote::Passage {
                text: "这里\n\nOriginal.".into()
            })
        );
    }

    #[test]
    fn paragraphs_split_on_blank_lines_only() {
        let text = "a\nb\n\n  \nc\n";
        let ranges = paragraphs(text);
        let parts: Vec<&str> = ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(parts, vec!["a\nb", "c"]);
    }
}
