//! The **Notebook** card (§13.1): the latest notes, bot-written ones marked,
//! and the full notebook in the panel.
//!
//! Every note says where it came from (§7): the owner, or the bot and the
//! thread or session it wrote from. The panel adds, edits and deletes
//! notes; an owner's edit makes a note the owner's (the engine resets its
//! source).

use super::board::{card_corner, card_item};
use super::items::{hover_row, section_head, section_rule};
use super::panel::{PanelHead, PanelItem};
use super::parts;
use super::*;
use cypher_proto::{BotNote, NoteSource};

/// Notes the card lists.
const CARD_NOTES: usize = 3;

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

/// Whose notes the panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum NoteFilter {
    #[default]
    All,
    Owner,
    Bot,
}

impl NoteFilter {
    fn keeps(self, note: &BotNote) -> bool {
        match self {
            Self::All => true,
            Self::Owner => note.source == NoteSource::Owner,
            Self::Bot => matches!(note.source, NoteSource::Bot { .. }),
        }
    }
}

/// The notebook panel's inputs: the add field, the note being edited and
/// whose notes it lists.
#[derive(Default)]
pub(super) struct NotebookUi {
    add: Option<(Entity<TextInput>, Subscription)>,
    /// The new-note editor is open (the header's + opened it).
    adding: bool,
    editing: Option<NoteEdit>,
    filter: NoteFilter,
}

impl Shell {
    /// The add field. Like the edit field it wraps and grows with the note:
    /// Enter adds, Shift+Enter breaks a line.
    fn note_add_input(&mut self, cx: &mut Context<Self>) -> Entity<TextInput> {
        if let Some((input, _)) = &self.board.notebook.add {
            return input.clone();
        }
        let input = cx.new(|cx| TextInput::new("Add a note…", cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.add_note(cx);
            }
        });
        self.board.notebook.add = Some((input.clone(), events));
        input
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
        self.board.notebook.adding = false;
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
        let input = cx.new(|cx| TextInput::new("Note", cx));
        input.update(cx, |input, cx| input.set_text(note.text.clone(), cx));
        window.focus(&input.focus_handle(cx), cx);
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.save_note_edit(cx);
            }
        });
        self.board.notebook.adding = false;
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
                PanelItem::Notebook,
                self.card_focused("notebook", cx),
                theme,
                cx,
            ));
        }
        let header = self.card_header(
            "notebook",
            "bot-notebook-header",
            icons::BOOK,
            "Notebook",
            notes.len(),
            PanelItem::Notebook,
            None,
            theme,
            cx,
        );
        let rows: Vec<_> = notes
            .iter()
            .take(CARD_NOTES)
            .map(|note| {
                // The bot's mark tells its notes from the owner's; who and
                // where from read in the panel.
                card_item(
                    &format!("bot-note-{}", note.id),
                    Some(note_mark(&note.source, bot, theme)),
                    // Its words, not its markdown.
                    transcript::single_line(&super::items::plain_markdown(&note.text)),
                    Some(card_corner(
                        super::items::ago(note.updated_at.max(note.created_at), Utc::now()),
                        None,
                        theme,
                    )),
                    false,
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.show_panel(PanelItem::Notebook, cx)))
            })
            .collect();
        Some(self.fold_card("notebook", header, rows, theme))
    }

    /// The detail panel: the add field, then the notes newest first under
    /// a heading that filters them by who wrote them.
    pub(super) fn render_notebook_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        window: &Window,
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
        let head = PanelHead {
            glyph: icons::BOOK,
            tint: None,
            title: "Notebook".into(),
        };
        // The header's one button: + opens a new note at the top.
        self.set_panel_actions(vec![
            super::items::icon_button("bot-note-new", icons::PLUS, "New note", theme)
                .on_click(cx.listener(|this, _, window, cx| this.open_new_note(window, cx)))
                .into_any_element(),
        ]);
        let mut body = div().p(px(8.0)).flex().flex_col();
        if self.board.notebook.adding {
            let input = self.note_add_input(cx);
            body = body.child(self.new_note_editor(&input, theme, cx));
        }
        if notes.is_empty() {
            if !self.board.notebook.adding {
                body = body.child(parts::quiet_line(
                    theme,
                    "No notes yet. Press + to add one, like \u{201C}The release waits on \
                     the migration\u{201D} or \u{201C}Alice owns auth\u{201D}.",
                ));
            }
            return Some((head, body));
        }

        // ── the notes, under a heading that filters them ──
        let filter = self.board.notebook.filter;
        let count = |f: NoteFilter| notes.iter().filter(|n| f.keeps(n)).count();
        let chips = [
            (NoteFilter::All, "All".to_string()),
            (NoteFilter::Owner, "You".to_string()),
            (NoteFilter::Bot, bot.name.clone()),
        ]
        .into_iter()
        .map(|(f, label)| {
            let key = format!("bot-notes-filter-{f:?}");
            let on = f == filter;
            div()
                .id(SharedString::from(key.clone()))
                .flex_none()
                .h(px(20.0))
                .px(px(7.0))
                .rounded(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .text_size(px(11.0))
                .bg(if on {
                    crate::kit::theme::wash(0.08)
                } else {
                    motion::hover_blend(
                        &key,
                        crate::kit::theme::wash(0.0),
                        crate::kit::theme::wash(0.05),
                    )
                })
                .on_hover(motion::hover_listener(key.clone()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board.notebook.filter = f;
                    cx.notify();
                }))
                .text_color(if on {
                    theme.text
                } else {
                    motion::hover_blend(&key, theme.text_muted, theme.text)
                })
                .child(SharedString::from(label))
                .child(
                    div()
                        .text_color(theme.text_faint)
                        .child(SharedString::from(count(f).to_string())),
                )
        });
        body = body
            .when(self.board.notebook.adding, |el| el.child(section_rule()))
            .child(
                section_head("Notes", !self.board.notebook.adding, theme)
                    .child(div().flex_1())
                    .child(
                        div()
                            .mr(px(-7.0))
                            .flex()
                            .flex_row()
                            .gap(px(2.0))
                            .children(chips),
                    ),
            );
        let shown: Vec<BotNote> = notes.into_iter().filter(|n| filter.keeps(n)).collect();
        if shown.is_empty() {
            body = body.child(parts::quiet_line(
                theme,
                match filter {
                    NoteFilter::Owner => "No notes from you yet.".to_string(),
                    _ => format!("No notes from {} yet.", bot.name),
                },
            ));
        }
        let editing = self
            .board
            .notebook
            .editing
            .as_ref()
            .map(|e| (e.note_id.clone(), e.input.clone()));
        for note in shown {
            let row = match &editing {
                Some((id, input)) if *id == note.id => {
                    let by_bot = matches!(note.source, NoteSource::Bot { .. });
                    self.note_editor(input, by_bot, theme, cx)
                }
                _ => self.note_row(&note, bot, theme, window, cx),
            };
            body = body.child(row);
        }
        Some((head, body))
    }

    /// One note in the panel: who wrote it, its text (markdown, justified),
    /// where it came from and when; on hover Copy, Edit and Delete. A
    /// click on the text edits it.
    fn note_row(
        &self,
        note: &BotNote,
        bot: &Bot,
        theme: &Theme,
        window: &Window,
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
                    .child(SharedString::from(format!("{} · {when}", bot.name)))
                    .when_some(from, |el, from| {
                        el.child("·").child(match chat_id.clone() {
                            Some(chat_id) => parts::link(format!("{key}-from"), from, theme)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_chat(chat_id.clone(), cx)
                                }))
                                .into_any_element(),
                            None => div().child(SharedString::from(from)).into_any_element(),
                        })
                    })
            }
        };
        let reveal = motion::hover_t(&key);
        let (edit_note, pen_note, delete_id) = (note.clone(), note.clone(), note.id.clone());
        let copy_key = format!("{key}-copy");
        let copied = self.board.copied.as_deref() == Some(copy_key.as_str());
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
                .child(
                    icon(glyph)
                        .size(px(13.0))
                        .text_color(motion::hover_blend(&id, color, theme.text)),
                )
        };
        let text = note.text.clone();
        let tools = div()
            .flex_none()
            .my(px(-5.0))
            .flex()
            .flex_row()
            .child(
                tool(
                    copy_key.clone(),
                    if copied { icons::CHECK } else { icons::COPY },
                    "Copy note",
                    if copied {
                        theme.success
                    } else {
                        theme.text_muted
                    },
                )
                .when(copied, |el| el.opacity(1.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy_text(copy_key.clone(), text.clone(), cx)
                })),
            )
            .child(
                tool(
                    format!("{key}-edit"),
                    icons::PEN,
                    "Edit note",
                    theme.text_muted,
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.edit_note(&pen_note, window, cx)),
                ),
            )
            .child(
                tool(
                    format!("{key}-delete"),
                    icons::TRASH_BIN_MINIMALISTIC,
                    "Delete note",
                    theme.text_muted,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board_mutate(
                        serde_json::json!({ "op": "deleteBotNote", "noteId": delete_id }),
                        cx,
                    );
                })),
            );
        // The tools ride the source line's end, so the text keeps the
        // row's whole width.
        hover_row(&key)
            .py(px(8.0))
            .child(div().pt(px(2.0)).child(note_mark(&note.source, bot, theme)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("{key}-text")))
                            .cursor_text()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_note(&edit_note, window, cx)
                            }))
                            .child(super::items::panel_markdown(
                                &note.text,
                                format!("{key}-md"),
                                13.0,
                                theme.text,
                                theme,
                                window,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                source
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(11.0))
                                    .line_height(px(15.0))
                                    .text_color(theme.text_muted.opacity(0.85)),
                            )
                            .child(tools),
                    ),
            )
            .into_any_element()
    }

    /// Open the new-note editor at the top of the panel, focused.
    fn open_new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.board.notebook.editing = None;
        self.board.notebook.adding = true;
        let input = self.note_add_input(cx);
        window.focus(&input.focus_handle(cx), cx);
        cx.notify();
    }

    /// The new note: its field, then Cancel and Add. Enter adds,
    /// Shift+Enter breaks a line.
    fn new_note_editor(
        &self,
        input: &Entity<TextInput>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .px(px(2.0))
            .pt(px(4.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(parts::field(input).mx(px(0.0)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "bot-note-new-cancel")
                            .id("bot-note-new-cancel")
                            .py(px(4.0))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.board.notebook.adding = false;
                                if let Some((input, _)) = &this.board.notebook.add {
                                    input.update(cx, |input, cx| input.set_text("", cx));
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(theme, "Add")
                            .id("bot-note-add")
                            .py(px(4.0))
                            .on_click(cx.listener(|this, _, _, cx| this.add_note(cx))),
                    ),
            )
            .into_any_element()
    }

    /// The edit field in a note's place. Saving a bot's note makes it the
    /// owner's (§7), and the editor says so.
    fn note_editor(
        &self,
        input: &Entity<TextInput>,
        by_bot: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Indented to the note text it replaces.
        div()
            .py(px(6.0))
            .pl(px(24.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(parts::field(input))
            .child(
                div()
                    .px(px(8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.5))
                            .text_color(theme.text_faint)
                            .when(by_bot, |el| el.child("Saving makes it your note.")),
                    )
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
            14.0,
            theme,
        )
        .into_any_element(),
        NoteSource::Owner => div()
            .size(px(14.0))
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
