//! The **Notebook** card (§13.1): the latest notes, bot-written ones marked,
//! and the full notebook in the panel.
//!
//! Every note says where it came from (§7): the owner, or the bot and the
//! thread or session it wrote from. The panel adds, edits and deletes
//! notes; an owner's edit makes a note the owner's (the engine resets its
//! source).

use super::board::{card, section_header};
use super::panel::{PanelHead, PanelItem};
use super::parts;
use super::*;
use cypher_proto::{BotNote, NoteSource};

/// Notes the card lists.
const CARD_NOTES: usize = 3;

/// A note's provenance line: "You · Oct 12", "Written by Bob · from Nightly
/// review", or "Written by Bob · Oct 12" when the bot named no source.
pub(super) fn provenance(source: &NoteSource, bot: &str, when: &str) -> String {
    match source {
        NoteSource::Owner => format!("You · {when}"),
        NoteSource::Bot {
            title: Some(title), ..
        } => {
            format!("Written by {bot} · from {}", transcript::single_line(title))
        }
        NoteSource::Bot { title: None, .. } => format!("Written by {bot} · {when}"),
    }
}

/// The bot's notes, newest first.
pub(super) fn notes_of<'a>(notes: &'a [BotNote], bot_id: &str) -> Vec<&'a BotNote> {
    let mut notes: Vec<&BotNote> = notes.iter().filter(|n| n.bot_id == bot_id).collect();
    notes.sort_by_key(|n| std::cmp::Reverse((n.updated_at.max(n.created_at), n.id.clone())));
    notes
}

struct NoteEdit {
    note_id: String,
    input: Entity<TextInput>,
    _events: Subscription,
}

/// The notebook panel's inputs: the add field and the note being edited.
#[derive(Default)]
pub(super) struct NotebookUi {
    add: Option<(Entity<TextInput>, Subscription)>,
    editing: Option<NoteEdit>,
}

impl Shell {
    fn note_add_input(&mut self, cx: &mut Context<Self>) -> Entity<TextInput> {
        if let Some((input, _)) = &self.board.notebook.add {
            return input.clone();
        }
        let input = cx.new(|cx| TextInput::settings_field("Add a note…", false, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.add_note(cx);
            }
        });
        self.board.notebook.add = Some((input.clone(), events));
        input
    }

    /// Open the notebook with the add field focused (the empty card's `+`).
    fn open_notebook_to_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.note_add_input(cx);
        window.focus(&input.focus_handle(cx), cx);
        self.show_panel(PanelItem::Notebook, cx);
    }

    fn add_note(&mut self, cx: &mut Context<Self>) {
        let Some((input, _)) = &self.board.notebook.add else {
            return;
        };
        let text = input.read(cx).text().trim().to_string();
        let bot_id = self.settings.botboard_bot.clone();
        let (Some(bot_id), false) = (bot_id, text.is_empty()) else {
            return;
        };
        input.update(cx, |input, cx| input.set_text("", cx));
        self.board_mutate(
            serde_json::json!({
                "op": "saveBotNote",
                "noteId": uuid::Uuid::new_v4().to_string(),
                "botId": bot_id,
                "text": text,
            }),
            cx,
        );
    }

    fn edit_note(&mut self, note: &BotNote, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::settings_field("Note", false, cx));
        input.update(cx, |input, cx| input.set_text(note.text.clone(), cx));
        window.focus(&input.focus_handle(cx), cx);
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.save_note_edit(cx);
            }
        });
        self.board.notebook.editing = Some(NoteEdit {
            note_id: note.id.clone(),
            input,
            _events: events,
        });
        cx.notify();
    }

    fn save_note_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.board.notebook.editing.take() else {
            return;
        };
        let text = edit.input.read(cx).text().trim().to_string();
        let note = self
            .state
            .read(cx)
            .bot_records
            .notes
            .iter()
            .find(|n| n.id == edit.note_id)
            .cloned();
        // Unchanged or emptied: nothing to write (Delete removes a note).
        if let Some(note) = note.filter(|n| !text.is_empty() && n.text != text) {
            self.board_mutate(
                serde_json::json!({
                    "op": "saveBotNote",
                    "noteId": note.id,
                    "botId": note.bot_id,
                    "text": text,
                }),
                cx,
            );
        }
        cx.notify();
    }

    /// The sidebar card: the latest notes, or the one-line way to add the
    /// first.
    pub(super) fn render_notebook_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notes: Vec<BotNote> = {
            let state = self.state.read(cx);
            notes_of(&state.bot_records.notes, &bot.id)
                .into_iter()
                .cloned()
                .collect()
        };
        if notes.is_empty() {
            return Some(parts::empty_card(
                "bot-notebook-empty",
                icons::BOOK,
                "Notebook",
                "No notes yet",
                "Add a note",
                PanelItem::Notebook,
                |this, window, cx| this.open_notebook_to_add(window, cx),
                theme,
                cx,
            ));
        }
        let open = self.board.panel.as_ref() == Some(&PanelItem::Notebook);
        let header = section_header(
            "bot-notebook-header",
            icons::BOOK,
            "Notebook",
            notes.len(),
            open,
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.open_panel(PanelItem::Notebook, cx)));
        let rows: Vec<_> = notes
            .iter()
            .take(CARD_NOTES)
            .map(|note| {
                let key = format!("bot-note-{}", note.id);
                let sub = provenance(
                    &note.source,
                    &bot.name,
                    &parts::past_ms(note.updated_at.max(note.created_at)),
                );
                div()
                    .id(SharedString::from(key.clone()))
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .cursor_pointer()
                    .bg(motion::hover_blend(
                        &key,
                        crate::kit::theme::wash(0.0),
                        crate::kit::theme::wash(0.05),
                    ))
                    .on_hover(motion::hover_listener(key.clone()))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.show_panel(PanelItem::Notebook, cx)),
                    )
                    .child(note_mark(&note.source, bot, theme))
                    .child(super::board::two_lines(
                        transcript::single_line(&note.text),
                        sub,
                        String::new(),
                        theme,
                    ))
            })
            .collect();
        Some(
            card(theme)
                .pb(px(4.0))
                .child(header)
                .child(div().px(px(4.0)).flex().flex_col().children(rows))
                .into_any_element(),
        )
    }

    /// The detail panel: the add field, then every note newest first.
    pub(super) fn render_notebook_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        if *item != PanelItem::Notebook {
            return None;
        }
        let notes: Vec<BotNote> = {
            let state = self.state.read(cx);
            notes_of(&state.bot_records.notes, &bot.id)
                .into_iter()
                .cloned()
                .collect()
        };
        let by_bot = notes
            .iter()
            .filter(|n| matches!(n.source, NoteSource::Bot { .. }))
            .count();
        let head = PanelHead {
            glyph: icons::BOOK,
            tint: None,
            title: "Notebook".into(),
            subtitle: match (notes.len(), by_bot) {
                (0, _) => format!("What {} keeps in mind", bot.name),
                (n, 0) => format!("{n} note{}", if n == 1 { "" } else { "s" }),
                (n, k) => format!(
                    "{n} note{} · {k} written by {}",
                    if n == 1 { "" } else { "s" },
                    bot.name
                ),
            },
        };
        let add = self.note_add_input(cx);
        let add_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(parts::field(&add).flex_1().min_w_0().mr(px(0.0)))
            .child(
                popover::btn_primary(theme, "Add")
                    .id("bot-note-add")
                    .mr(px(8.0))
                    .py(px(5.0))
                    .on_click(cx.listener(|this, _, _, cx| this.add_note(cx))),
            );
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(div().pt(px(4.0)).child(add_row))
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(6.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!(
                        "Short facts and decisions {} reads at the start of each conversation.",
                        bot.name
                    ))),
            );
        if notes.is_empty() {
            body = body.child(div().h(px(8.0))).child(parts::quiet_line(
                theme,
                "No notes yet. Try \u{201C}The release waits on the migration\u{201D} \
                 or \u{201C}Alice owns auth\u{201D}.",
            ));
            return Some((head, body));
        }
        body = body.child(parts::section_label(theme, "Notes"));
        let editing = self
            .board
            .notebook
            .editing
            .as_ref()
            .map(|e| (e.note_id.clone(), e.input.clone()));
        for note in notes {
            let row = match &editing {
                Some((id, input)) if *id == note.id => self.note_editor(input, theme, cx),
                _ => self.note_row(&note, bot, theme, cx),
            };
            body = body.child(row);
        }
        Some((head, body))
    }

    /// One note in the panel: its text, its source and time, and on hover
    /// Edit and Delete. A click on the text edits it.
    fn note_row(
        &self,
        note: &BotNote,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("bot-panel-note-{}", note.id);
        let when = parts::past_full_ms(note.updated_at.max(note.created_at));
        let source = match &note.source {
            NoteSource::Owner => div().child(SharedString::from(format!("You · {when}"))),
            NoteSource::Bot { chat_id, title } => {
                let from = title.clone().map(|t| transcript::single_line(&t));
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(4.0))
                    .child(SharedString::from(format!("Written by {}", bot.name)))
                    .when_some(from, |el, from| {
                        el.child("· from").child(match chat_id.clone() {
                            Some(chat_id) => parts::link(format!("{key}-from"), from, theme)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_chat(chat_id.clone(), cx)
                                }))
                                .into_any_element(),
                            None => div().child(SharedString::from(from)).into_any_element(),
                        })
                    })
                    .child(SharedString::from(format!("· {when}")))
            }
        };
        let reveal = motion::hover_t(&key);
        let (edit_note, delete_id) = (note.clone(), note.id.clone());
        let tool = |id: String, glyph: &'static str, tip: &'static str, color: gpui::Hsla| {
            div()
                .id(SharedString::from(id.clone()))
                .size(px(24.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .opacity(reveal)
                .bg(motion::hover_blend(
                    &id,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.12),
                ))
                .on_hover(motion::hover_listener(id.clone()))
                .tooltip(move |_, cx| {
                    cx.new(|_| super::super::session::FindTooltip(tip.into()))
                        .into()
                })
                .child(icon(glyph).size(px(13.0)).text_color(color))
        };
        div()
            .id(SharedString::from(key.clone()))
            .px(px(8.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_start()
            .gap(px(10.0))
            .bg(motion::hover_blend(
                &key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.04),
            ))
            .on_hover(motion::hover_listener(key.clone()))
            .child(div().pt(px(1.0)).child(note_mark(&note.source, bot, theme)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("{key}-text")))
                            .cursor_text()
                            .text_size(px(13.0))
                            .line_height(px(19.0))
                            .text_color(theme.text)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_note(&edit_note, window, cx)
                            }))
                            .child(SharedString::from(note.text.clone())),
                    )
                    .child(
                        source
                            .text_size(px(11.0))
                            .line_height(px(15.0))
                            .text_color(theme.text_muted.opacity(0.85)),
                    ),
            )
            .child(
                div().flex_none().flex().flex_row().child(
                    tool(
                        format!("{key}-delete"),
                        icons::TRASH_BIN_MINIMALISTIC,
                        "Delete note",
                        theme.text_muted,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board_mutate(
                            serde_json::json!({
                                "op": "deleteBotNote",
                                "noteId": delete_id,
                            }),
                            cx,
                        );
                    })),
                ),
            )
            .into_any_element()
    }

    fn note_editor(
        &self,
        input: &Entity<TextInput>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .py(px(6.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(parts::field(input))
            .child(
                div()
                    .px(px(8.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "bot-note-edit-cancel")
                            .id("bot-note-edit-cancel")
                            .py(px(4.0))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.board.notebook.editing = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(theme, "Save")
                            .id("bot-note-edit-save")
                            .py(px(4.0))
                            .on_click(cx.listener(|this, _, _, cx| this.save_note_edit(cx))),
                    ),
            )
            .into_any_element()
    }
}

/// A note's mark: the bot's own avatar for what it wrote, a page for the
/// owner's.
fn note_mark(source: &NoteSource, bot: &Bot, theme: &Theme) -> AnyElement {
    match source {
        NoteSource::Bot { .. } => crate::appearance::bot_style::bot_avatar(
            &bot.name,
            bot.icon.as_deref(),
            bot.color.as_deref(),
            16.0,
            theme,
        )
        .into_any_element(),
        NoteSource::Owner => div()
            .size(px(16.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                icon(icons::DOCUMENT)
                    .size(px(14.0))
                    .text_color(theme.text_muted.opacity(0.8)),
            )
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, bot: &str, at: i64, source: NoteSource) -> BotNote {
        BotNote {
            id: id.into(),
            bot_id: bot.into(),
            text: id.into(),
            source,
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn provenance_names_the_writer_and_source() {
        assert_eq!(
            provenance(&NoteSource::Owner, "Bob", "Oct 12"),
            "You · Oct 12"
        );
        assert_eq!(
            provenance(
                &NoteSource::Bot {
                    chat_id: Some("c".into()),
                    title: Some("Nightly\nreview".into()),
                },
                "Bob",
                "Oct 12"
            ),
            "Written by Bob · from Nightly review"
        );
        assert_eq!(
            provenance(
                &NoteSource::Bot {
                    chat_id: None,
                    title: None
                },
                "Bob",
                "09:12"
            ),
            "Written by Bob · 09:12"
        );
    }

    #[test]
    fn notes_list_newest_first_for_their_bot() {
        let mut edited = note("edited", "b", 1, NoteSource::Owner);
        edited.updated_at = 50;
        let notes = vec![
            note("old", "b", 10, NoteSource::Owner),
            note("other", "x", 99, NoteSource::Owner),
            edited,
            note("new", "b", 30, NoteSource::Owner),
        ];
        let ids: Vec<&str> = notes_of(&notes, "b")
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(ids, ["edited", "new", "old"]);
    }
}
