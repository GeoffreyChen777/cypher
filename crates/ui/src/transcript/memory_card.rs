//! The held memory write card (docs/design/memory.md §7): a `memory` call
//! whose write was held, because its run read untrusted content, asks the
//! owner right where it was made. Save / Don't save while it waits, then a
//! one-line record of the answer. A memory call nothing was held for stays
//! an ordinary chip.
//!
//! Which calls are cards comes from the stores' rows ([`MemoryCards`]):
//! each carries its newest held writes and recent decisions by tool call.
//! The rows are built with that set, and their versions carry its key, so a
//! decision redraws (and re-measures) the card.

use std::collections::HashSet;

use super::*;
use cypher_proto::view::{BotToolKind, bot_tool_kind};
use cypher_proto::{BotRecords, HeldWrite, MemoryEdit, MemoryScope, MemoryStore};

/// Lines of the proposed text a card shows before it fades out, and their
/// height.
const PREVIEW_LINES: usize = 6;
const PREVIEW_LINE_H: f32 = 18.0;
/// Characters past which the shown lines likely wrap beyond the box.
const PREVIEW_WRAP_CHARS: usize = 300;

/// The `memory` calls drawn as cards: those with a held write or a
/// decision on one, in any store; and a hash of what the cards show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryCards {
    ids: HashSet<String>,
    pub key: u64,
}

impl MemoryCards {
    pub(super) fn of(records: &BotRecords) -> Self {
        let mut ids = HashSet::new();
        let mut acc: Vec<u8> = Vec::new();
        for store in &records.memory_stores {
            for held in &store.held {
                if let Some(id) = &held.tool_call_id {
                    ids.insert(id.clone());
                    acc.extend_from_slice(held.id.as_bytes());
                    acc.extend_from_slice(id.as_bytes());
                }
            }
            for decided in &store.decided {
                if let Some(id) = &decided.tool_call_id {
                    ids.insert(id.clone());
                    acc.extend_from_slice(id.as_bytes());
                    acc.push(u8::from(decided.saved));
                }
            }
        }
        let key = if acc.is_empty() { 0 } else { fnv1a(&acc) };
        Self { ids, key }
    }

    pub(super) fn contains(&self, call_id: &str) -> bool {
        self.ids.contains(call_id)
    }

    /// A hash of the cards among `entry`'s calls, for its row cache.
    pub(super) fn entry_key(&self, entry: &SessionMessageEntry) -> u64 {
        if self.ids.is_empty() {
            return 0;
        }
        entry
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Tool { id, .. } if self.ids.contains(id) => Some(fnv1a(id.as_bytes())),
                _ => None,
            })
            .fold(0, |acc, h| acc.rotate_left(5) ^ h)
    }
}

/// Whether a tool part is a `memory` write drawn as a card.
pub(super) fn is_card_call(part: &MessagePart, cards: &MemoryCards) -> bool {
    matches!(
        part,
        MessagePart::Tool { id, call, .. }
            if cards.contains(id)
                && matches!(
                    bot_tool_kind(call),
                    Some(
                        BotToolKind::CreateMemory
                            | BotToolKind::UpdateMemory
                            | BotToolKind::DeleteMemory
                            | BotToolKind::RenameMemory
                            | BotToolKind::Memory
                    )
                )
    )
}

/// The file a `memory` call wrote, by its last path segment (a rename's
/// new name).
pub(super) fn call_file(call: &ToolCall) -> String {
    let ToolCall::Unknown {
        input: Some(input), ..
    } = call
    else {
        return String::new();
    };
    ["path", "new_path"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
        .map(file_name)
        .unwrap_or_default()
}

/// A path's last segment: `/project/decisions/rounding.md` → `rounding.md`.
fn file_name(path: &str) -> String {
    let path = path.trim().trim_end_matches('/');
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Where a card stands.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum CardState<'a> {
    /// Waiting on the owner, in this store.
    Held {
        store: &'a MemoryStore,
        write: &'a HeldWrite,
    },
    Decided {
        saved: bool,
    },
}

/// The state of the card for `call_id`: still held in some store, else
/// decided; `None` once neither row remembers it.
pub(super) fn card_state<'a>(records: &'a BotRecords, call_id: &str) -> Option<CardState<'a>> {
    let stores = &records.memory_stores;
    stores
        .iter()
        .find_map(|store| {
            store
                .held_for_call(call_id)
                .map(|write| CardState::Held { store, write })
        })
        .or_else(|| {
            stores.iter().find_map(|store| {
                store
                    .decided_for_call(call_id)
                    .map(|d| CardState::Decided { saved: d.saved })
            })
        })
}

/// The proposed text as the card shows it: its first lines, and whether
/// there is more (more lines, a preview the row clipped, or lines long
/// enough to wrap past the box).
pub(super) fn preview_lines(text: &str) -> (String, bool) {
    let text = text.trim_end();
    let lines: Vec<&str> = text.lines().collect();
    let shown = lines[..lines.len().min(PREVIEW_LINES)].join("\n");
    let more = lines.len() > PREVIEW_LINES
        || text.ends_with('…')
        || shown.chars().count() > PREVIEW_WRAP_CHARS;
    (shown, more)
}

/// Why it was held, as a sentence: "Held because this session fetched a
/// web page (https://…)".
pub(super) fn reason_line(reason: &str) -> String {
    let reason = reason.trim().trim_end_matches('.');
    if reason.is_empty() {
        "Held because this session read untrusted content".into()
    } else {
        format!("Held because this session {reason}")
    }
}

/// Which memory a store is, mid-sentence: "your memory", "invoice-demo's
/// memory".
pub(super) fn store_words(scope: &MemoryScope, project: Option<&str>) -> String {
    match scope {
        MemoryScope::You => "your memory".into(),
        MemoryScope::Project { .. } => match project {
            Some(name) => format!("{name}'s memory"),
            None => "the project's memory".into(),
        },
        MemoryScope::Workspace { .. } => "the workspace's memory".into(),
        MemoryScope::Role { agent, .. } => format!("{agent}'s memory"),
    }
}

/// A decided card's one line, like a resolved question's chip.
fn decided_line(saved: bool, file: &str, theme: &Theme) -> AnyElement {
    let (glyph, label, tone) = if saved {
        (crate::kit::icons::CHECK, "Saved to memory", theme.success)
    } else {
        (crate::kit::icons::BOOK, "Not saved", theme.text_muted)
    };
    div()
        .py(px(4.0))
        .w_full()
        .child(
            div()
                .h(px(34.0))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .rounded(px(10.0))
                .border_1()
                .border_color(crate::kit::theme::hairline(0.08))
                .bg(crate::kit::theme::ink(0.045))
                .px(px(8.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            crate::kit::icons::icon(glyph)
                                .size(px(12.0))
                                .text_color(tone),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(theme.text.opacity(0.9))
                        .child(SharedString::from(file.to_string())),
                ),
        )
        .into_any_element()
}

impl Transcript {
    /// The card for the `memory` call `call_id` (see the module docs).
    pub(super) fn render_memory_card(
        &mut self,
        row_id: &SharedString,
        kind: &RowKind,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let RowKind::MemoryWrite { call_id, file } = kind else {
            return gpui::Empty.into_any_element();
        };
        let (scope, write, store) = {
            let state = self.state.read(cx);
            match card_state(&state.bot_records, call_id) {
                Some(CardState::Held { store, write }) => {
                    let project = match &store.scope {
                        MemoryScope::Project { space_id } => state
                            .space_row(space_id)
                            .map(|s| s.display_name().to_string()),
                        _ => None,
                    };
                    (
                        store.scope.clone(),
                        write.clone(),
                        store_words(&store.scope, project.as_deref()),
                    )
                }
                Some(CardState::Decided { saved }) => {
                    return decided_line(saved, file, theme);
                }
                // Left both rows; the next sync draws the chip again.
                None => return gpui::Empty.into_any_element(),
            }
        };
        let name = file_name(&write.path);
        let busy = self.memory_busy.contains(&write.id);
        let error = self.memory_errors.get(&write.id).cloned();

        // ── the head: the tile in Needs-you amber, the question and where ──
        let head = div()
            .px(px(16.0))
            .pt(px(13.0))
            .pb(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_none()
                    .size(px(28.0))
                    .rounded(px(8.0))
                    .bg(theme.warning.opacity(0.14))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        crate::kit::icons::icon(crate::kit::icons::BOOK)
                            .size(px(14.0))
                            .text_color(theme.warning),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.5))
                            .line_height(px(18.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from("Save to memory?")),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!("{name} · in {store}"))),
                    ),
            );

        // ── what it would write, a few lines, fading out when there is more ──
        let proposed = match write.preview.as_deref() {
            Some(text) if !text.trim().is_empty() => {
                let (shown, more) = preview_lines(text);
                let fade_to = theme.input_glass_bg();
                div()
                    .relative()
                    .overflow_hidden()
                    .max_h(px(PREVIEW_LINES as f32 * PREVIEW_LINE_H + 18.0))
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(9.0))
                    .bg(crate::kit::theme::ink(0.04))
                    .mono(theme)
                    .text_size(px(12.0))
                    .line_height(px(PREVIEW_LINE_H))
                    .text_color(theme.text.opacity(0.9))
                    .child(SharedString::from(shown))
                    .when(more, |el| {
                        el.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(28.0))
                                .bg(gpui::linear_gradient(
                                    180.0,
                                    gpui::linear_color_stop(fade_to.opacity(0.0), 0.0),
                                    gpui::linear_color_stop(fade_to.opacity(0.9), 1.0),
                                )),
                        )
                    })
            }
            Some(_) => quiet(theme, "Leaves the file empty."),
            None => quiet(theme, "Deletes the file."),
        };
        let body = div()
            .px(px(16.0))
            .pt(px(12.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(proposed)
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(theme.text_muted.opacity(0.85))
                    .child(SharedString::from(reason_line(&write.reason))),
            );

        // ── Don't save / Save, and what went wrong ──
        let (save_id, discard_id) = (write.id.clone(), write.id.clone());
        let (save_scope, discard_scope) = (scope.clone(), scope);
        let save = crate::kit::popover::btn_primary(theme, "Save")
            .id(SharedString::from(format!("{row_id}-save")))
            .py(px(5.0))
            .when(!busy, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    this.decide_held(
                        save_scope.clone(),
                        MemoryEdit::Accept {
                            id: save_id.clone(),
                        },
                        cx,
                    )
                }))
            });
        let discard =
            crate::kit::popover::btn_ghost(theme, "Don't save", format!("{row_id}-discard"))
                .id(SharedString::from(format!("{row_id}-discard")))
                .py(px(5.0))
                .when(!busy, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        this.decide_held(
                            discard_scope.clone(),
                            MemoryEdit::Discard {
                                id: discard_id.clone(),
                            },
                            cx,
                        )
                    }))
                });
        let (hint, hint_color) = match &error {
            Some(error) => (error.clone(), theme.danger),
            None => (
                SharedString::from("Not read by any run until you save it."),
                theme.text_muted,
            ),
        };
        let actions = div()
            .px(px(16.0))
            .py(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(hint_color)
                    .child(hint),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .when(busy, |el| el.opacity(0.6))
                    .child(discard)
                    .child(save),
            );
        let card = div()
            .flex()
            .flex_col()
            .child(head)
            .child(super::bot_plan::plan_rule())
            .child(body)
            .child(super::bot_plan::plan_rule())
            .child(actions);
        div()
            .py(px(4.0))
            .w_full()
            .child(super::bot_plan::plan_surface(theme, card))
            .into_any_element()
    }

    /// Answer a held write. On success the store's row moves it to its
    /// decisions and the card follows; until then its buttons stay off.
    fn decide_held(&mut self, scope: MemoryScope, edit: MemoryEdit, cx: &mut Context<Self>) {
        let id = match &edit {
            MemoryEdit::Accept { id } | MemoryEdit::Discard { id } => id.clone(),
            _ => return,
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.memory_errors.insert(id, "Engine not connected".into());
            return self.remeasure_memory_cards(cx);
        };
        if !self.memory_busy.insert(id.clone()) {
            return;
        }
        self.memory_errors.remove(&id);
        self.remeasure_memory_cards(cx);
        let params = serde_json::json!({ "scope": scope, "edit": edit });
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(cypher_rpc::methods::EDIT_MEMORY, params)
                .await;
            this.update(cx, |this: &mut Transcript, cx| {
                if let Err(err) = result {
                    this.memory_busy.remove(&id);
                    this.memory_errors
                        .insert(id, error_sentence(&err.to_string()));
                    this.remeasure_memory_cards(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-measure the memory cards: an error line came or went.
    fn remeasure_memory_cards(&mut self, cx: &mut Context<Self>) {
        for (ix, row) in self.rows.iter().enumerate() {
            if matches!(row.kind, RowKind::MemoryWrite { .. }) {
                self.list.remeasure_items(ix..ix + 1);
            }
        }
        cx.notify();
    }
}

/// A quiet line in place of the proposed text.
fn quiet(theme: &Theme, text: &'static str) -> gpui::Div {
    div()
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .child(SharedString::from(text))
}

/// An RPC error as a sentence, without the transport's lead.
fn error_sentence(text: &str) -> SharedString {
    let text = text
        .strip_prefix("bad params: ")
        .or_else(|| text.strip_prefix("transport: "))
        .unwrap_or(text)
        .trim();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>().into(),
        None => "Couldn't reach memory.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::HeldDecision;

    fn held(id: &str, call: Option<&str>) -> HeldWrite {
        HeldWrite {
            id: id.into(),
            chat_id: "chat".into(),
            tool_call_id: call.map(Into::into),
            path: "/decisions/rounding.md".into(),
            preview: Some("Round half up.".into()),
            reason: "fetched a web page (https://example.com)".into(),
            at: 0,
        }
    }

    fn records() -> BotRecords {
        let mut you = MemoryStore::new("u".into(), MemoryScope::You, "d".into(), 0);
        you.held = vec![held("h1", Some("call-1")), held("h0", None)];
        let mut project = MemoryStore::new(
            "p".into(),
            MemoryScope::Project {
                space_id: "web".into(),
            },
            "d".into(),
            0,
        );
        project.decided = vec![
            HeldDecision {
                id: "h2".into(),
                tool_call_id: Some("call-2".into()),
                saved: false,
                at: 1,
            },
            HeldDecision {
                id: "h3".into(),
                tool_call_id: Some("call-3".into()),
                saved: true,
                at: 2,
            },
        ];
        BotRecords {
            memory_stores: vec![you, project],
            ..Default::default()
        }
    }

    fn memory_part(id: &str, command: &str) -> MessagePart {
        MessagePart::Tool {
            id: id.into(),
            call: ToolCall::Unknown {
                name: "memory".into(),
                input: Some(serde_json::json!({
                    "command": command, "path": "/you/decisions/rounding.md",
                })),
            },
            is_error: false,
            resolved: true,
            output: None,
            progress: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            permission: None,
        }
    }

    #[test]
    fn a_card_waits_while_held_then_shows_the_answer() {
        let records = records();
        match card_state(&records, "call-1") {
            Some(CardState::Held { store, write }) => {
                assert_eq!(store.scope, MemoryScope::You);
                assert_eq!(write.id, "h1");
            }
            other => panic!("expected held, got {other:?}"),
        }
        assert_eq!(
            card_state(&records, "call-2"),
            Some(CardState::Decided { saved: false })
        );
        assert_eq!(
            card_state(&records, "call-3"),
            Some(CardState::Decided { saved: true })
        );
        assert_eq!(card_state(&records, "call-9"), None);
    }

    #[test]
    fn only_memory_writes_with_a_row_are_cards() {
        let records = records();
        let cards = MemoryCards::of(&records);
        assert!(cards.contains("call-1") && cards.contains("call-2"));
        assert_ne!(cards.key, 0);
        assert_eq!(MemoryCards::of(&BotRecords::default()).key, 0);
        assert!(is_card_call(&memory_part("call-1", "create"), &cards));
        assert!(is_card_call(&memory_part("call-3", "str_replace"), &cards));
        assert!(
            !is_card_call(&memory_part("call-1", "view"), &cards),
            "a read is never held"
        );
        assert!(
            !is_card_call(&memory_part("call-9", "create"), &cards),
            "nothing held: the chip"
        );
        // A decision changes the key, so the rows rebuild.
        let mut decided = records.clone();
        decided.memory_stores[0].held.remove(0);
        decided.memory_stores[0].decided.push(HeldDecision {
            id: "h1".into(),
            tool_call_id: Some("call-1".into()),
            saved: true,
            at: 3,
        });
        assert_ne!(MemoryCards::of(&decided).key, cards.key);
    }

    #[test]
    fn a_held_write_is_a_row_of_its_own() {
        let entry = SessionMessageEntry {
            role: MessageRole::Assistant,
            parts: vec![
                memory_part("read", "view"),
                memory_part("call-1", "create"),
                memory_part("plain", "create"),
            ],
            ..crate::test_fixtures::entry()
        };
        let cards = MemoryCards::of(&records());
        let mut parse = |_: &str, text: &str| Arc::new(parse_full(text));
        let kinds: Vec<&str> = entry_rows(&entry, false, &cards, &mut parse)
            .iter()
            .map(|row| match &row.kind {
                RowKind::ToolGroup { .. } => "group",
                RowKind::Activity { .. } => "activity",
                RowKind::MemoryWrite { call_id, file } => {
                    assert_eq!((call_id.as_ref(), file.as_ref()), ("call-1", "rounding.md"));
                    "card"
                }
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["group", "card", "group"]);
        assert_ne!(cards.entry_key(&entry), 0, "its rows cache with the card");
    }

    #[test]
    fn cards_name_the_file_the_reason_and_the_store() {
        let MessagePart::Tool { call, .. } = memory_part("c", "create") else {
            unreachable!()
        };
        assert_eq!(call_file(&call), "rounding.md");
        let rename = ToolCall::Unknown {
            name: "memory".into(),
            input: Some(serde_json::json!({
                "command": "rename", "old_path": "/you/a.md", "new_path": "/you/b.md",
            })),
        };
        assert_eq!(call_file(&rename), "b.md");
        assert_eq!(
            reason_line("fetched a web page (https://example.com)."),
            "Held because this session fetched a web page (https://example.com)"
        );
        assert_eq!(
            reason_line(" "),
            "Held because this session read untrusted content"
        );
        assert_eq!(store_words(&MemoryScope::You, None), "your memory");
        let project = MemoryScope::Project {
            space_id: "web".into(),
        };
        assert_eq!(store_words(&project, Some("Web")), "Web's memory");
    }

    #[test]
    fn previews_show_a_few_lines_and_say_when_there_is_more() {
        assert_eq!(preview_lines("one\ntwo\n"), ("one\ntwo".into(), false));
        let long = (1..=9)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(preview_lines(&long), ("1\n2\n3\n4\n5\n6".into(), true));
        assert!(
            preview_lines(&"x".repeat(PREVIEW_WRAP_CHARS + 1)).1,
            "wraps"
        );
        assert!(preview_lines("cut off…").1, "the row clipped it");
    }
}
