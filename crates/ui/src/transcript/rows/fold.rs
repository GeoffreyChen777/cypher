//! Folding: collapsing finished work runs and thoughts, nesting tool calls
//! under their parent, the open/closed toggle state, and capping long runs
//! behind an overflow row.

use std::collections::HashMap;

use cypher_doc::SessionMessageEntry;
use cypher_proto::ToolCall;
use gpui::SharedString;

use crate::markdown::parser::{Block, BlockTree};
use crate::transcript::{hidden_tool_count, single_line};

use super::{Row, RowKind, ToolItem, fnv1a, is_nested};

/// Fold a closed work run — `rows[start..]`, opened by part `first_part` —
/// behind one [`RowKind::Activity`] row when it mixed tool calls with
/// thinking. A run of only tools is one group already and a lone thought its
/// own toggle; both stay as they are.
pub(super) fn fold_work_run(
    rows: &mut Vec<Row>,
    start: usize,
    first_part: &str,
    entry: &SessionMessageEntry,
    auto_open: bool,
) {
    let mut thoughts = 0usize;
    let mut tools: Vec<(ToolCall, bool)> = Vec::new();
    for row in &rows[start..] {
        match &row.kind {
            RowKind::Thought { .. } => thoughts += 1,
            RowKind::ToolGroup { tools: group, .. } => {
                tools.extend(group.iter().map(|t| (t.call.clone(), t.is_error)));
            }
            _ => {}
        }
    }
    if thoughts == 0 || tools.is_empty() {
        return;
    }
    for row in &mut rows[start..] {
        if let RowKind::ToolGroup { nested, .. }
        | RowKind::Thought { nested, .. }
        | RowKind::ThoughtBlock { nested, .. } = &mut row.kind
        {
            *nested = true;
            // A thought that gains its first tool call redraws as a chip.
            row.version ^= 1 << 61;
        }
    }
    let summary = cypher_proto::view::work_summary(&tools, thoughts);
    let count = rows.len() - start;
    rows.insert(
        start,
        Row {
            id: format!("{}#{}.activity", entry.id, first_part).into(),
            version: (fnv1a(summary.as_bytes()) ^ count as u64) << 1 | auto_open as u64,
            turn_start: false,
            kind: RowKind::Activity {
                rows: count,
                summary: summary.into(),
                auto_open,
            },
            entry_id: entry.id.clone().into(),
            role: entry.role,
            timestamp: None,
            answered: None,
        },
    );
}

/// A thought's label in a work run: its first line, without the heading or
/// emphasis markers models often title a thought with ("**Planning**").
pub(in crate::transcript) fn thought_preview(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut line = line.trim_start_matches('#').trim_start();
    for marker in ["**", "__", "*", "_"] {
        if line.len() > marker.len() * 2 && line.starts_with(marker) && line.ends_with(marker) {
            line = &line[marker.len()..line.len() - marker.len()];
            break;
        }
    }
    single_line(line)
}

/// Order a tool group so each call made from inside another call's run
/// follows that call, one level deeper. Pi runs the calls a codemode script
/// makes (`await tools.read(…)`) through its own tool pipeline and reports
/// each as a tool call whose id is `{caller id}/{n}`; the doc keeps those ids,
/// so the nesting needs no field of its own — old transcripts nest too, and
/// viewers that predate this list the same calls flat.
///
/// A call whose caller is not in the group (a different group, or an id that
/// merely contains `/`) stays where it is at depth 0. Siblings keep their
/// arrival order.
pub(in crate::transcript) fn nest_tool_calls(items: Vec<(String, ToolItem)>) -> Vec<ToolItem> {
    let index: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(ix, (id, _))| (id.as_str(), ix))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (ix, (id, _)) in items.iter().enumerate() {
        // A caller's id is a strict prefix of its calls' ids, so this never
        // cycles.
        match id
            .rsplit_once('/')
            .and_then(|(caller, _)| index.get(caller).copied())
        {
            Some(caller) => children[caller].push(ix),
            None => roots.push(ix),
        }
    }
    if roots.len() == items.len() {
        return items.into_iter().map(|(_, item)| item).collect();
    }
    let mut order: Vec<(usize, u8)> = Vec::with_capacity(items.len());
    let mut stack: Vec<(usize, u8)> = roots.iter().rev().map(|&ix| (ix, 0)).collect();
    while let Some((ix, depth)) = stack.pop() {
        order.push((ix, depth));
        for &child in children[ix].iter().rev() {
            stack.push((child, depth.saturating_add(1)));
        }
    }
    let mut slots: Vec<Option<ToolItem>> = items.into_iter().map(|(_, item)| Some(item)).collect();
    order
        .into_iter()
        .filter_map(|(ix, depth)| {
            let mut item = slots[ix].take()?;
            item.depth = depth;
            Some(item)
        })
        .collect()
}

/// How many top-level blocks of an append-mode translation's `tree` belong to
/// the folded original: the original's own blocks plus the separator rule.
/// `None` unless `text` is `agent`, the separator and a translation that has
/// begun — until then (and whenever the rendering doesn't parse as expected,
/// e.g. an original ending inside an open code fence) the text shows whole.
pub(super) fn appended_original_blocks(text: &str, agent: &str, tree: &BlockTree) -> Option<usize> {
    crate::quote_origin::appended_translation(text, agent)?;
    let rule = tree
        .blocks
        .iter()
        .position(|top| top.range.start >= agent.len())?;
    (rule > 0 && rule + 1 < tree.blocks.len() && matches!(tree.blocks[rule].block, Block::Rule))
        .then_some(rule + 1)
}

/// The rows a toggle covers and whether it starts open: a translation's
/// original and a thought start closed, a work run only while it streams.
fn toggle_span(kind: &RowKind) -> Option<(usize, bool)> {
    match kind {
        RowKind::TranslationOriginal { blocks } | RowKind::Thought { blocks, .. } => {
            Some((*blocks, false))
        }
        RowKind::Activity {
            rows, auto_open, ..
        } => Some((*rows, *auto_open)),
        _ => None,
    }
}

/// Whether the toggle `row` is open: the user's click (`pins`, by row id)
/// wins, else the toggle's default.
pub(in crate::transcript) fn toggle_open(row: &Row, pins: &HashMap<SharedString, bool>) -> bool {
    toggle_span(&row.kind).is_some_and(|(_, default)| pins.get(&row.id).copied().unwrap_or(default))
}

/// Drop the rows each CLOSED toggle covers — a translation's original, a
/// thought, a work run. `pins` holds the toggles the user clicked, by row id;
/// every other toggle keeps its default ([`toggle_span`]). A folded row's
/// timestamp (a reply that ended while thinking) moves onto its toggle, so
/// the entry keeps its strip.
pub fn fold_closed_toggles(rows: &mut Vec<Row>, pins: &HashMap<SharedString, bool>) {
    let mut hide = 0usize;
    let mut folded: Vec<Row> = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        if hide > 0 {
            hide -= 1;
            if let (Some(stamp), Some(toggle)) = (row.timestamp, folded.last_mut()) {
                toggle.timestamp = Some(stamp);
                toggle.version ^= 1 << 62;
                if let Some(label) = row.answered {
                    toggle.version ^= fnv1a(label.text.as_bytes()).rotate_left(1)
                        ^ u64::from(label.substituted.is_some());
                    toggle.answered = Some(label);
                }
            }
            continue;
        }
        if let Some((covered, _)) = toggle_span(&row.kind)
            && !toggle_open(&row, pins)
        {
            hide = covered;
        }
        folded.push(row);
    }
    *rows = folded;
}

/// Cap each open work run the way a tool group caps its chips: keep the
/// run's LAST `limit` tool calls ([`hidden_tool_count`]) and fold everything
/// before them — earlier calls and thoughts alike — behind one
/// [`RowKind::RunOverflow`] row. The cut lands right after the last hidden
/// call, so the thinking that led into the first kept one stays; a cut inside
/// a tool group hides that group's leading chips (`skip`). `revealed` holds
/// the runs the user unfolded, by [`RowKind::Activity`] row id: they show
/// whole, under a row that folds them back.
///
/// Runs after [`fold_closed_toggles`]: a closed run has no rows left to cap.
pub fn cap_work_runs(
    rows: &mut Vec<Row>,
    limit: u32,
    revealed: &std::collections::HashSet<SharedString>,
) {
    let mut ix = 0;
    while ix < rows.len() {
        if !matches!(rows[ix].kind, RowKind::Activity { .. }) {
            ix += 1;
            continue;
        }
        let run = rows[ix].id.clone();
        let start = ix + 1;
        let end = start
            + rows[start..]
                .iter()
                .take_while(|row| is_nested(&row.kind))
                .count();
        let total: usize = rows[start..end]
            .iter()
            .map(|row| match &row.kind {
                RowKind::ToolGroup { tools, .. } => tools.len(),
                _ => 0,
            })
            .sum();
        let is_revealed = revealed.contains(&run);
        let hidden = hidden_tool_count(total, limit, is_revealed);
        if hidden == 0 && !(is_revealed && hidden_tool_count(total, limit, false) > 0) {
            ix = end;
            continue;
        }
        // `rows[start..cut]` fold away whole; `skip` chips of `rows[cut]` too.
        let (mut cut, mut left, mut skip, mut thoughts) = (start, hidden, 0, 0);
        while left > 0 {
            match &rows[cut].kind {
                RowKind::ToolGroup { tools, .. } if tools.len() > left => {
                    skip = left;
                    break;
                }
                RowKind::ToolGroup { tools, .. } => left -= tools.len(),
                RowKind::Thought { .. } => thoughts += 1,
                _ => {}
            }
            cut += 1;
        }
        let group = &mut rows[cut];
        if let RowKind::ToolGroup {
            skip: group_skip, ..
        } = &mut group.kind
            && skip > 0
        {
            *group_skip = skip;
            group.version ^= (skip as u64) << 48;
        }
        let overflow = Row {
            id: format!("{run}.overflow").into(),
            version: (hidden as u64) << 32 | thoughts as u64,
            turn_start: false,
            kind: RowKind::RunOverflow {
                run,
                tools: hidden,
                thoughts,
            },
            entry_id: rows[ix].entry_id.clone(),
            role: rows[ix].role,
            timestamp: None,
            answered: None,
        };
        rows.splice(start..cut, [overflow]);
        ix = end - (cut - start) + 1;
    }
}

/// The label of a work run's [`RowKind::RunOverflow`] row: what it folds
/// away, or — for a revealed run (`tools == 0`) — that it folds it back.
pub(in crate::transcript) fn run_overflow_label(tools: usize, thoughts: usize) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    match (tools, thoughts) {
        (0, _) => "Show fewer tool calls".to_string(),
        (tools, 0) => format!("Show {tools} earlier tool call{}", plural(tools)),
        (tools, thoughts) => format!(
            "Show {tools} earlier tool call{} and {thoughts} thought{}",
            plural(tools),
            plural(thoughts)
        ),
    }
}
