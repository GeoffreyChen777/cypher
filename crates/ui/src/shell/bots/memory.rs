//! **Memory** (docs/design/memory.md §8): what agents learn and keep
//! between sessions, as a store of markdown files. One pane draws a store
//! wherever it opens: a workspace's own (its sidebar card and panel), a
//! project's (from the project's page), and yours and every project's in
//! Settings → Memory ([`crate::settings::memory`]).
//!
//! A store's page reads top to bottom: writes held for review, `MEMORY.md`
//! rendered, the other files, consolidation, then the recent changes, each
//! undoable. A file opens on a page of its own to read, edit, copy or
//! delete. The pane watches its store (`WatchMemory`) while it lives; every
//! edit is one `EditMemory` call, whose sentence shows on the page for a
//! moment. Its host lays the page out ([`MemoryParts`]) and follows the
//! pages it asks for ([`MemoryPaneEvent`]).

use super::items::{arrow_in, head_note, hover_row, panel_markdown, section_head, section_rule};
use super::panel::{PanelHead, PanelItem};
use super::parts::{self, CallError};
use super::*;
use cypher_proto::{
    MEMORY_INDEX, MemoryAction, MemoryChange, MemoryEdit, MemoryFile, MemoryPending, MemoryScope,
    MemoryStore, MemoryView, PassOutcome,
};

/// Changes a store's page lists, newest first.
const CHANGES_SHOWN: usize = 20;
/// How long a reply's sentence stays on the page.
const NOTE_MS: u64 = 4000;
/// How long an armed Delete waits for its confirming click.
const DELETE_ARM_MS: u64 = 4000;
/// How long the hour field waits after typing before it saves.
const HOUR_SAVE_MS: u64 = 700;

/// Which page of a store the pane draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryPage {
    Index,
    File(String),
    /// The new-file form.
    New,
}

/// What the pane asks of its host.
pub(crate) enum MemoryPaneEvent {
    /// Show this page: a file row opened, a file was made or deleted.
    Show(MemoryPage),
}

/// A page as drawn, for its host to lay out: its body, the header's
/// buttons, a pinned footer (an editor's Save) and, for an empty store,
/// the message to centre in place of the body.
pub(crate) struct MemoryParts {
    pub body: gpui::Div,
    pub actions: Vec<AnyElement>,
    pub footer: Option<AnyElement>,
    pub empty: Option<AnyElement>,
}

/// A file being written: an existing one, or a new one with its path.
struct FileEditor {
    /// `None` for a new file.
    path: Option<String>,
    name: Option<Entity<TextInput>>,
    text: Entity<TextInput>,
    error: Option<SharedString>,
    busy: bool,
    _events: Vec<Subscription>,
}

/// One store's page and file pages (see the module docs).
pub(crate) struct MemoryPane {
    state: Entity<crate::state::AppState>,
    scope: MemoryScope,
    /// The store as last seen; `None` until its first frame.
    view: Option<MemoryView>,
    watch: Option<Task<()>>,
    editor: Option<FileEditor>,
    /// A file just created, until a frame carries it.
    created: Option<String>,
    /// The consolidation hour field, and the save it waits to make.
    hour: Option<(Entity<TextInput>, Subscription)>,
    hour_save: Option<Task<()>>,
    /// Edits on their way, by the key of the button that made them.
    busy: std::collections::HashSet<String>,
    /// The last reply's sentence, or what went wrong (`true`).
    note: Option<(SharedString, bool)>,
    note_reset: Option<Task<()>>,
    /// The file whose Delete is armed, and the task that disarms it.
    delete_armed: Option<String>,
    delete_disarm: Option<Task<()>>,
    copied: bool,
    copied_reset: Option<Task<()>>,
    _observe: Subscription,
}

impl gpui::EventEmitter<MemoryPaneEvent> for MemoryPane {}

impl MemoryPane {
    pub(crate) fn new(
        state: Entity<crate::state::AppState>,
        scope: MemoryScope,
        cx: &mut Context<Self>,
    ) -> Self {
        // Redrawn with the store's row and the names it uses; the watch
        // starts once there is an engine to ask.
        let observe = cx.observe(&state, |pane: &mut Self, _, cx| {
            pane.ensure_watch(cx);
            cx.notify();
        });
        let mut pane = Self {
            state,
            scope,
            view: None,
            watch: None,
            editor: None,
            created: None,
            hour: None,
            hour_save: None,
            busy: Default::default(),
            note: None,
            note_reset: None,
            delete_armed: None,
            delete_disarm: None,
            copied: false,
            copied_reset: None,
            _observe: observe,
        };
        pane.ensure_watch(cx);
        pane
    }

    pub(crate) fn scope(&self) -> &MemoryScope {
        &self.scope
    }

    /// The store's row: the frame's when one came, else the registry's
    /// (`None` for a store nothing has written yet).
    fn store(&self, cx: &App) -> Option<MemoryStore> {
        self.view.as_ref().map(|v| v.store.clone()).or_else(|| {
            self.state
                .read(cx)
                .bot_records
                .memory_store(&self.scope)
                .cloned()
        })
    }

    /// Follow the store's frames, resubscribing when the stream ends (an
    /// engine restart) like the standing watches.
    fn ensure_watch(&mut self, cx: &mut Context<Self>) {
        if self.watch.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({ "scope": self.scope });
        self.watch = Some(cx.spawn(async move |this, cx| {
            const RETRY_DELAY: Duration = Duration::from_secs(2);
            loop {
                match engine
                    .client()
                    .subscribe(methods::WATCH_MEMORY, params.clone())
                    .await
                {
                    Ok(mut rx) => {
                        while let Some(value) = rx.recv().await {
                            let view: MemoryView = match serde_json::from_value(value) {
                                Ok(view) => view,
                                Err(err) => {
                                    tracing::warn!(error = %err, "dropping malformed memory frame");
                                    continue;
                                }
                            };
                            let alive =
                                this.update(cx, |pane, cx| {
                                    if pane.created.as_ref().is_some_and(|path| {
                                        view.files.iter().any(|f| f.path == *path)
                                    }) {
                                        pane.created = None;
                                    }
                                    pane.view = Some(view);
                                    cx.notify();
                                });
                            if alive.is_err() {
                                return;
                            }
                        }
                    }
                    Err(err) => tracing::debug!(error = %err, "memory watch unavailable; retrying"),
                }
                if this.update(cx, |_, _| {}).is_err() {
                    return;
                }
                cx.background_executor().timer(RETRY_DELAY).await;
            }
        }));
    }

    /// Send `edit`; `key` names the button that waits on it. The reply's
    /// sentence (or the error) shows on the page; `done` runs on success.
    fn edit(
        &mut self,
        key: String,
        edit: MemoryEdit,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.say("Engine not connected", true, cx);
            return;
        };
        if !self.busy.insert(key.clone()) {
            return;
        }
        let params = serde_json::json!({ "scope": self.scope, "edit": edit });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::EDIT_MEMORY, params)
                .await
                .map_err(CallError::from);
            this.update(cx, |pane, cx| {
                pane.busy.remove(&key);
                match result {
                    Ok(reply) => {
                        if let Some(text) = reply.get("text").and_then(|t| t.as_str()) {
                            pane.say(text.to_string(), false, cx);
                        }
                        done(pane, cx);
                    }
                    Err(err) => pane.say(err.to_string(), true, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show `text` on the page; a reply fades after a moment, an error stays
    /// until the next one.
    fn say(&mut self, text: impl Into<SharedString>, error: bool, cx: &mut Context<Self>) {
        self.note = Some((text.into(), error));
        self.note_reset = (!error).then(|| {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(NOTE_MS))
                    .await;
                this.update(cx, |pane, cx| {
                    pane.note = None;
                    cx.notify();
                })
                .ok();
            })
        });
        cx.notify();
    }

    fn show(&mut self, page: MemoryPage, cx: &mut Context<Self>) {
        cx.emit(MemoryPaneEvent::Show(page));
    }

    /// The page for `page`.
    pub(crate) fn render_page(
        &mut self,
        page: &MemoryPage,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> MemoryParts {
        let theme = Theme::of(cx).clone();
        let mut parts = match page {
            MemoryPage::Index => self.render_index(&theme, window, cx),
            MemoryPage::File(path) => self.render_file(path, &theme, window, cx),
            MemoryPage::New => self.render_new(&theme, cx),
        };
        if let Some((text, error)) = self.note.clone() {
            parts.body = div()
                .flex()
                .flex_col()
                .child(
                    parts::quiet_line(&theme, text)
                        .text_size(px(12.0))
                        .text_color(if error {
                            theme.danger
                        } else {
                            theme.text_muted
                        }),
                )
                .child(parts.body);
        }
        parts
    }

    // ── the store's page ────────────────────────────────────────────────────

    fn render_index(
        &mut self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> MemoryParts {
        let new = super::items::icon_button("memory-new-file", icons::PLUS, "New file", theme)
            .on_click(cx.listener(|pane, _, window, cx| pane.start_new(window, cx)))
            .into_any_element();
        let mut parts = MemoryParts {
            body: div().p(px(8.0)).flex().flex_col(),
            actions: vec![new],
            footer: None,
            empty: None,
        };
        let store = self.store(cx);
        let Some(view) = self.view.clone() else {
            // No frame yet: a store nothing wrote is empty; one with a row
            // is on its way.
            match store {
                Some(_) => parts.body = parts.body.child(parts::quiet_line(theme, "Loading…")),
                None => parts.empty = Some(self.empty_message(theme)),
            }
            return parts;
        };
        if view.files.is_empty() && view.pending.is_empty() && view.changes.is_empty() {
            parts.empty = Some(self.empty_message(theme));
            return parts;
        }
        let mut body = parts.body;
        let mut first = true;
        let mut next_section = |body: gpui::Div, label: &str| {
            let body = if first {
                body
            } else {
                body.child(section_rule())
            };
            let head = section_head(label, first, theme);
            first = false;
            (body, head)
        };

        // ── held writes, first: they wait on the owner ──
        if !view.pending.is_empty() {
            let (b, head) = next_section(body, "To review");
            body = b.child(
                head.child(div().flex_1())
                    .child(head_note("From runs that read untrusted content", theme)),
            );
            let mut pending = view.pending.clone();
            pending.sort_by_key(|p| p.at);
            for write in &pending {
                body = body.child(self.pending_row(write, theme, window, cx));
            }
        }

        // ── MEMORY.md, read as the prompt reads it ──
        let index = view.files.iter().find(|f| f.path == MEMORY_INDEX);
        let (b, head) = next_section(body, "MEMORY.md");
        let mut head = head.child(div().flex_1());
        if let Some(file) = index {
            head = head.child(head_note(by_line(file), theme));
        }
        let edit_index = small_button("memory-index-edit", icons::PEN, "Edit", theme).on_click(
            cx.listener(|pane, _, window, cx| {
                pane.start_edit(MEMORY_INDEX, window, cx);
                pane.show(MemoryPage::File(MEMORY_INDEX.into()), cx);
            }),
        );
        body = b.child(head.child(edit_index));
        body = match index.filter(|f| !f.text.trim().is_empty()) {
            Some(file) => body.child(div().px(px(8.0)).child(panel_markdown(
                &file.text,
                "memory-index-md",
                13.0,
                theme.text,
                theme,
                window,
            ))),
            None => body.child(parts::quiet_line(
                theme,
                "Empty. It's the index every run reads first; the other files are read on \
                 demand.",
            )),
        };

        // ── the other files ──
        let others: Vec<&MemoryFile> = view
            .files
            .iter()
            .filter(|f| f.path != MEMORY_INDEX)
            .collect();
        if !others.is_empty() {
            let (b, head) = next_section(body, "Files");
            let bytes: usize = others.iter().map(|f| f.text.len()).sum();
            body = b.child(head.child(div().flex_1()).child(head_note(
                format!(
                    "{} · {}",
                    count(others.len(), "file", "files"),
                    size_label(bytes)
                ),
                theme,
            )));
            for file in others {
                body = body.child(self.file_row(file, theme, cx));
            }
        }

        // ── consolidation ──
        if let Some(store) = &store {
            let (b, head) = next_section(body, "Consolidation");
            body = b.child(head.child(div().flex_1()).child(head_note(
                if store.consolidation.enabled {
                    format!("Nightly at {}", hour_label(store.consolidation.hour))
                } else {
                    "Off".to_string()
                },
                theme,
            )));
            body = self.consolidation(body, store, theme, cx);
        }

        // ── what changed, newest first ──
        if !view.changes.is_empty() {
            let (b, head) = next_section(body, "Changes");
            body = b.child(head.child(div().flex_1()).child(head_note(
                count(view.changes.len(), "change", "changes"),
                theme,
            )));
            for change in view.changes.iter().take(CHANGES_SHOWN) {
                body = body.child(self.change_row(change, &view.files, theme, cx));
            }
        }
        parts.body = body.pb(px(16.0));
        parts
    }

    fn empty_message(&self, theme: &Theme) -> AnyElement {
        let hint = match &self.scope {
            MemoryScope::You => {
                "Agents keep what holds across your projects here, like how you like your \
                 answers. Every session and workspace reads it."
            }
            MemoryScope::Project { .. } => {
                "Sessions in this project keep what they learn here: its commands, pitfalls \
                 and decisions."
            }
            MemoryScope::Workspace { .. } => {
                "The workspace keeps what it learns here: who owns what, standing decisions, \
                 what to watch."
            }
            MemoryScope::Role { .. } => "This agent keeps what it learns here.",
        };
        parts::panel_empty(theme, icons::BOOK, "No memory yet", hint).into_any_element()
    }

    /// A held write: where and why, the file as it would be, Accept and
    /// Discard.
    fn pending_row(
        &self,
        write: &MemoryPending,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (accept_key, discard_key) = (
            format!("memory-accept-{}", write.id),
            format!("memory-discard-{}", write.id),
        );
        let busy = self.busy.contains(&accept_key) || self.busy.contains(&discard_key);
        let (accept_id, discard_id) = (write.id.clone(), write.id.clone());
        let preview = match write.preview.as_deref() {
            Some(text) if !text.trim().is_empty() => div()
                .mx(px(8.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(crate::kit::theme::ink(0.03))
                .border_1()
                .border_color(crate::kit::theme::hairline(0.06))
                .child(panel_markdown(
                    text,
                    format!("memory-pending-{}", write.id),
                    12.5,
                    theme.text.opacity(0.9),
                    theme,
                    window,
                )),
            Some(_) => parts::quiet_line(theme, "Leaves the file empty."),
            None => parts::quiet_line(theme, "Deletes the file."),
        };
        div()
            .py(px(4.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .px(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.0))
                                    .line_height(px(18.0))
                                    .text_color(theme.text)
                                    .child(SharedString::from(file_name(&write.path))),
                            )
                            .child(super::items::trailing(
                                parts::ago_ms(write.at),
                                theme.text_faint,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(16.0))
                            .text_color(theme.text_muted.opacity(0.85))
                            .child(SharedString::from(format!(
                                "{} · {}",
                                write.author.label(),
                                write.reason.trim()
                            ))),
                    ),
            )
            .child(preview)
            .child(
                div()
                    .px(px(8.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(6.0))
                    .when(busy, |el| el.opacity(0.6))
                    .child(
                        popover::btn_ghost(theme, "Discard", discard_key.clone())
                            .id(SharedString::from(discard_key.clone()))
                            .py(px(4.0))
                            .when(!busy, |el| {
                                el.on_click(cx.listener(move |pane, _, _, cx| {
                                    pane.edit(
                                        discard_key.clone(),
                                        MemoryEdit::Discard {
                                            id: discard_id.clone(),
                                        },
                                        cx,
                                        |_, _| {},
                                    )
                                }))
                            }),
                    )
                    .child(
                        popover::btn_primary(theme, "Accept")
                            .id(SharedString::from(accept_key.clone()))
                            .py(px(4.0))
                            .when(!busy, |el| {
                                el.on_click(cx.listener(move |pane, _, _, cx| {
                                    pane.edit(
                                        accept_key.clone(),
                                        MemoryEdit::Accept {
                                            id: accept_id.clone(),
                                        },
                                        cx,
                                        |_, _| {},
                                    )
                                }))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// A file on the list: its path, size and who last changed it; a click
    /// opens it.
    fn file_row(&self, file: &MemoryFile, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("memory-file-{}", file.path);
        let path = file.path.clone();
        hover_row(&key)
            .cursor_pointer()
            .on_click(
                cx.listener(move |pane, _, _, cx| pane.show(MemoryPage::File(path.clone()), cx)),
            )
            .child(
                div().pt(px(2.0)).child(
                    icon(icons::DOCUMENT)
                        .size(px(14.0))
                        .flex_none()
                        .text_color(theme.text_muted.opacity(0.8)),
                ),
            )
            .child(two_line(
                file.path.trim_start_matches('/').to_string(),
                format!("{} · {}", size_label(file.text.len()), by_line(file)),
                None,
                theme,
            ))
            .child(div().pt(px(5.0)).child(arrow_in(&key, theme)))
            .into_any_element()
    }

    /// The nightly pass: its switch and hour, how the last pass went and
    /// what passes cost this month, then Run now and Undo.
    fn consolidation(
        &mut self,
        body: gpui::Div,
        store: &MemoryStore,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let setting = store.consolidation;
        let switch_key = "memory-consolidation".to_string();
        let mut group = parts::group_box().child(
            parts::switch_row(
                switch_key.clone(),
                "Nightly pass",
                "Rereads recent sessions to merge, correct and prune what's here. Skipped \
                 when nothing new happened.",
                setting.enabled,
                true,
                theme,
            )
            .on_click(cx.listener(move |pane, _, _, cx| {
                pane.edit(
                    switch_key.clone(),
                    MemoryEdit::SetConsolidation {
                        enabled: !setting.enabled,
                        hour: setting.hour,
                    },
                    cx,
                    |_, _| {},
                )
            })),
        );
        if setting.enabled {
            let input = self.hour_input(setting.hour, cx);
            let typed = input.read(cx).text().to_string();
            let invalid = parse_hour(&typed).is_none();
            group = group.child(
                div()
                    .border_t_1()
                    .border_color(crate::kit::theme::hairline(0.06))
                    .h(px(42.0))
                    .px(px(12.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.5))
                            .text_color(if invalid { theme.danger } else { theme.text })
                            .child(if invalid {
                                "Use an hour, like 03:00"
                            } else {
                                "Time"
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(64.0))
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(7.0))
                            .border_1()
                            .border_color(crate::kit::theme::hairline(0.08))
                            .bg(crate::kit::theme::ink(0.04))
                            .text_size(px(12.5))
                            .child(input.into_any_element()),
                    ),
            );
        }
        let mut body = body.child(group).child(div().h(px(6.0)));
        if let Some(pass) = &store.last_pass {
            let (word, color) = match pass.outcome {
                PassOutcome::Running => ("Running", theme.text),
                PassOutcome::Done => ("Done", theme.text),
                PassOutcome::Skipped => ("Skipped", theme.text_muted),
                PassOutcome::Failed => ("Failed", theme.danger),
                PassOutcome::Undone => ("Undone", theme.text_muted),
            };
            let mark = match pass.outcome {
                PassOutcome::Done => Some(super::items::status_glyph(icons::CHECK, theme.success)),
                PassOutcome::Failed => Some(super::items::status_glyph(
                    icons::DANGER_TRIANGLE,
                    theme.danger,
                )),
                _ => None,
            };
            let mut line = div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(5.0))
                .children(mark)
                .child(div().text_color(color).child(word))
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(format!(
                            "· {}{}",
                            parts::past_full_ms(pass.ended_at.unwrap_or(pass.at)),
                            if pass.manual { " · run by hand" } else { "" }
                        ))),
                );
            if let Some(cost) = pass_cost(pass.changes, pass.tokens) {
                line = line.child(
                    div()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(format!("· {cost}"))),
                );
            }
            let summary = match pass.outcome {
                PassOutcome::Skipped => Some("Nothing new since the last pass.".to_string()),
                _ => pass.summary.clone().filter(|s| !s.trim().is_empty()),
            };
            body = body.child(parts::fact(
                theme,
                "Last pass",
                div().flex().flex_col().gap(px(3.0)).child(line).when_some(
                    summary,
                    |el, summary| {
                        el.child(
                            div()
                                .text_size(px(12.0))
                                .line_height(px(17.0))
                                .text_color(theme.text_muted)
                                .child(super::items::text_value(&summary)),
                        )
                    },
                ),
            ));
        }
        if let Some(month) = store.month.as_ref().filter(|m| m.passes > 0) {
            body = body.child(parts::fact(
                theme,
                "This month",
                SharedString::from(format!(
                    "{} · {}",
                    tokens_label(month.tokens),
                    count(month.passes as usize, "pass", "passes")
                )),
            ));
        }
        // Run now, unless a pass runs or was just asked for; Undo for a
        // finished pass that kept the version before it.
        let waiting = pass_waiting(store);
        let run_key = "memory-run-pass".to_string();
        let run = parts::action(
            run_key.clone(),
            icons::PLAY,
            if waiting { "Running…" } else { "Run now" },
            if waiting {
                theme.text_muted
            } else {
                theme.text
            },
        )
        .when(waiting || self.busy.contains(&run_key), |el| {
            el.opacity(0.6).cursor_default()
        })
        .when(!waiting, |el| {
            el.on_click(cx.listener(move |pane, _, _, cx| {
                pane.edit(run_key.clone(), MemoryEdit::RunPass, cx, |_, _| {})
            }))
        });
        let undo = store
            .last_pass
            .as_ref()
            .filter(|p| p.outcome == PassOutcome::Done && p.undo.is_some())
            .map(|pass| {
                let (key, pass_id) = ("memory-undo-pass".to_string(), pass.id.clone());
                parts::action(key.clone(), icons::RESTART, "Undo this pass", theme.text).on_click(
                    cx.listener(move |pane, _, _, cx| {
                        pane.edit(
                            key.clone(),
                            MemoryEdit::UndoPass {
                                pass_id: pass_id.clone(),
                            },
                            cx,
                            |_, _| {},
                        )
                    }),
                )
            });
        body.child(
            div()
                .px(px(2.0))
                .pt(px(4.0))
                .flex()
                .flex_row()
                .gap(px(2.0))
                .child(run)
                .children(undo),
        )
    }

    /// The hour field, made on first use and seeded with the store's hour.
    /// A valid hour saves a moment after typing stops, or at once on Enter.
    fn hour_input(&mut self, hour: u8, cx: &mut Context<Self>) -> Entity<TextInput> {
        if let Some((input, _)) = &self.hour {
            return input.clone();
        }
        let input = cx.new(|cx| TextInput::settings_field("03:00", false, cx));
        input.update(cx, |input, cx| input.set_text(hour_label(hour), cx));
        let events = cx.subscribe(&input, |pane: &mut Self, input, event, cx| {
            let Some(hour) = parse_hour(input.read(cx).text()) else {
                pane.hour_save = None;
                return cx.notify();
            };
            match event {
                TextInputEvent::Submitted => pane.save_hour(hour, cx),
                TextInputEvent::Edited => {
                    pane.hour_save = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(HOUR_SAVE_MS))
                            .await;
                        this.update(cx, |pane, cx| pane.save_hour(hour, cx)).ok();
                    }));
                }
                _ => {}
            }
            cx.notify();
        });
        self.hour = Some((input.clone(), events));
        input
    }

    fn save_hour(&mut self, hour: u8, cx: &mut Context<Self>) {
        self.hour_save = None;
        let Some(store) = self.store(cx) else {
            return;
        };
        if !store.consolidation.enabled || store.consolidation.hour == hour {
            return;
        }
        self.edit(
            "memory-hour".into(),
            MemoryEdit::SetConsolidation {
                enabled: true,
                hour,
            },
            cx,
            |_, _| {},
        );
    }

    /// One change: what it did to which file, who and when, and Undo on
    /// hover while it can still be put back. A click opens the file.
    fn change_row(
        &self,
        change: &MemoryChange,
        files: &[MemoryFile],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("memory-change-{}", change.id);
        let undo_key = format!("{key}-undo");
        let exists = files.iter().any(|f| f.path == change.path);
        let mut sub = format!("{} · {}", change.author.label(), parts::ago_ms(change.at));
        if change.undone {
            sub.push_str(" · undone");
        }
        let undo = undoable(change).then(|| {
            let (id, busy) = (change.id.clone(), self.busy.contains(&undo_key));
            let reveal = motion::hover_t(&key);
            parts::action(undo_key.clone(), icons::RESTART, "Undo", theme.text_muted)
                .py(px(3.0))
                .opacity(if busy { 0.6 } else { reveal })
                .on_click(cx.listener(move |pane, _, _, cx| {
                    cx.stop_propagation();
                    pane.edit(
                        undo_key.clone(),
                        MemoryEdit::Undo {
                            change_id: id.clone(),
                        },
                        cx,
                        |_, _| {},
                    )
                }))
        });
        let path = change.path.clone();
        hover_row(&key)
            .when(exists, |el| {
                el.cursor_pointer().on_click(
                    cx.listener(move |pane, _, _, cx| {
                        pane.show(MemoryPage::File(path.clone()), cx)
                    }),
                )
            })
            .when(change.undone, |el| el.opacity(0.6))
            .child(
                div().pt(px(2.0)).child(
                    icon(action_glyph(&change.action))
                        .size(px(13.0))
                        .flex_none()
                        .text_color(theme.text_muted.opacity(0.8)),
                ),
            )
            .child(two_line(
                change_title(change),
                sub,
                change
                    .owner_text
                    .then(|| ("edited your text".to_string(), theme.warning)),
                theme,
            ))
            .children(undo.map(|undo| div().my(px(-2.0)).child(undo)))
            .into_any_element()
    }

    // ── one file ────────────────────────────────────────────────────────────

    fn render_file(
        &mut self,
        path: &str,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> MemoryParts {
        let file = self
            .view
            .as_ref()
            .and_then(|v| v.files.iter().find(|f| f.path == path).cloned());
        let mut parts = MemoryParts {
            body: div().p(px(8.0)).flex().flex_col(),
            actions: Vec::new(),
            footer: None,
            empty: None,
        };
        let editing = self
            .editor
            .as_ref()
            .is_some_and(|e| e.path.as_deref() == Some(path));
        if editing {
            return self.render_editor(parts, theme, cx);
        }
        let Some(file) = file else {
            // Gone (deleted elsewhere), or its frame is on its way.
            let text = match self.view {
                Some(_) if self.created.as_deref() != Some(path) => "This file is gone.",
                _ => "Loading…",
            };
            parts.body = parts.body.child(parts::quiet_line(theme, text));
            return parts;
        };
        parts.actions = self.file_actions(&file, theme, cx);
        let path = file.path.clone();
        parts.body = parts
            .body
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(4.0))
                    .pb(px(10.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_muted.opacity(0.85))
                    .child(SharedString::from(format!(
                        "{} · {} · {}",
                        size_label(file.text.len()),
                        file.author.label(),
                        parts::past_full_ms(file.updated_at.max(file.created_at)),
                    ))),
            )
            .child(match file.text.trim().is_empty() {
                true => parts::quiet_line(theme, "Empty.").into_any_element(),
                false => div()
                    .id(SharedString::from(format!("memory-file-text-{path}")))
                    .px(px(8.0))
                    .cursor_text()
                    .on_click(
                        cx.listener(move |pane, event: &gpui::ClickEvent, window, cx| {
                            // A double click starts editing, as in a document.
                            if event.click_count() >= 2 {
                                pane.start_edit(&path, window, cx);
                            }
                        }),
                    )
                    .child(panel_markdown(
                        &file.text,
                        format!("memory-file-md-{}", file.path),
                        13.0,
                        theme.text,
                        theme,
                        window,
                    ))
                    .into_any_element(),
            })
            .pb(px(16.0));
        parts
    }

    /// A file's header buttons: Copy, Edit, and Delete, which asks once
    /// more by turning red until a second click within a moment.
    fn file_actions(
        &self,
        file: &MemoryFile,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let button = super::items::icon_button;
        let text = file.text.clone();
        let copy = button(
            "memory-file-copy",
            if self.copied {
                icons::CHECK
            } else {
                icons::COPY
            },
            if self.copied { "Copied" } else { "Copy" },
            theme,
        )
        .on_click(cx.listener(move |pane, _, _, cx| pane.copy(text.clone(), cx)));
        let edit_path = file.path.clone();
        let edit = button("memory-file-edit", icons::PEN, "Edit", theme).on_click(
            cx.listener(move |pane, _, window, cx| pane.start_edit(&edit_path, window, cx)),
        );
        let armed = self.delete_armed.as_deref() == Some(file.path.as_str());
        let delete_path = file.path.clone();
        let delete = button(
            "memory-file-delete",
            icons::TRASH_BIN_MINIMALISTIC,
            if armed {
                "Click again to delete"
            } else {
                "Delete"
            },
            theme,
        )
        .when(armed, |el| el.bg(theme.danger.opacity(0.14)))
        .on_click(cx.listener(move |pane, _, _, cx| pane.arm_or_delete(delete_path.clone(), cx)));
        vec![
            copy.into_any_element(),
            edit.into_any_element(),
            delete.into_any_element(),
        ]
    }

    fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied = true;
        self.copied_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1600))
                .await;
            this.update(cx, |pane, cx| {
                pane.copied = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn arm_or_delete(&mut self, path: String, cx: &mut Context<Self>) {
        if self.delete_armed.as_deref() == Some(path.as_str()) {
            self.delete_armed = None;
            self.delete_disarm = None;
            self.edit(
                "memory-file-delete".into(),
                MemoryEdit::Delete { path },
                cx,
                |pane, cx| pane.show(MemoryPage::Index, cx),
            );
            return cx.notify();
        }
        self.delete_armed = Some(path.clone());
        self.delete_disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(DELETE_ARM_MS))
                .await;
            this.update(cx, |pane, cx| {
                if pane.delete_armed.as_deref() == Some(path.as_str()) {
                    pane.delete_armed = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    // ── writing ─────────────────────────────────────────────────────────────

    /// Edit `path` in place: its text in a field that grows with it.
    fn start_edit(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .view
            .as_ref()
            .and_then(|v| v.files.iter().find(|f| f.path == path))
            .map(|f| f.text.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| TextInput::settings_prompt_field("Markdown", cx));
        input.update(cx, |input, cx| input.set_text(text, cx));
        window.focus(&input.focus_handle(cx), cx);
        self.editor = Some(FileEditor {
            path: Some(path.to_string()),
            name: None,
            text: input,
            error: None,
            busy: false,
            _events: Vec::new(),
        });
        cx.notify();
    }

    /// The new-file form, its name field focused; Enter there moves on to
    /// the text.
    fn start_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor.as_ref().is_some_and(|e| e.path.is_none()) {
            let name = cx.new(|cx| TextInput::settings_field("build-and-test", false, cx));
            let text = cx.new(|cx| {
                TextInput::settings_prompt_field("What's worth keeping, in markdown.", cx)
            });
            let events =
                vec![
                    cx.subscribe_in(&name, window, |pane: &mut Self, _, event, window, cx| {
                        if matches!(event, TextInputEvent::Submitted)
                            && let Some(editor) = &pane.editor
                        {
                            window.focus(&editor.text.focus_handle(cx), cx);
                        }
                        if let Some(editor) = pane.editor.as_mut() {
                            editor.error = None;
                        }
                        cx.notify();
                    }),
                ];
            self.editor = Some(FileEditor {
                path: None,
                name: Some(name),
                text,
                error: None,
                busy: false,
                _events: events,
            });
        }
        if let Some(name) = self.editor.as_ref().and_then(|e| e.name.as_ref()) {
            window.focus(&name.focus_handle(cx), cx);
        }
        self.show(MemoryPage::New, cx);
        cx.notify();
    }

    fn render_new(&mut self, theme: &Theme, cx: &mut Context<Self>) -> MemoryParts {
        let parts = MemoryParts {
            body: div().p(px(8.0)).flex().flex_col(),
            actions: Vec::new(),
            footer: None,
            empty: None,
        };
        if !self.editor.as_ref().is_some_and(|e| e.path.is_none()) {
            // Opened some other way (restored): the + makes the form.
            return MemoryParts {
                body: parts
                    .body
                    .child(parts::quiet_line(theme, "Press + to start a new file.")),
                ..parts
            };
        }
        self.render_editor(parts, theme, cx)
    }

    /// The open editor in `parts`: the name field (a new file), the text,
    /// and Cancel / Save pinned at the foot.
    fn render_editor(
        &mut self,
        mut parts: MemoryParts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> MemoryParts {
        let Some(editor) = self.editor.as_ref() else {
            return parts;
        };
        let new = editor.path.is_none();
        let mut body = parts.body;
        if let Some(name) = &editor.name {
            let typed = name.read(cx).text().to_string();
            let hint = match memory_path(&typed) {
                Some(path) => format!("Saved as {path}"),
                None if typed.trim().is_empty() => {
                    "A topic file, linked from MEMORY.md. Folders are fine: decisions/rounding."
                        .to_string()
                }
                None => "Not usable: no part of it may be empty or start with a dot.".to_string(),
            };
            body = body
                .child(parts::field_label(theme, "Name"))
                .child(parts::field(name))
                .child(
                    div()
                        .px(px(8.0))
                        .pt(px(5.0))
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(hint)),
                )
                .child(parts::field_label(theme, "Text"));
        }
        body = body.child(parts::field(&editor.text).when(!new, |el| el.mt(px(4.0))));
        let (footer, cancel, save) = parts::form_footer(
            theme,
            "memory-editor",
            editor.error.clone(),
            match (new, editor.busy) {
                (_, true) => "Saving…",
                (true, false) => "Create",
                (false, false) => "Save",
            },
            editor.busy,
        );
        parts.footer = Some(
            footer
                .child(cancel.on_click(cx.listener(|pane, _, _, cx| pane.close_editor(cx))))
                .child(save.on_click(cx.listener(|pane, _, _, cx| pane.save_editor(cx))))
                .into_any_element(),
        );
        parts.body = body.pb(px(16.0));
        parts
    }

    /// Drop the editor; a file that was never written goes back to the
    /// store's page.
    fn close_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.take() {
            let written = editor.path.is_some_and(|path| {
                self.view
                    .as_ref()
                    .is_some_and(|v| v.files.iter().any(|f| f.path == path))
            });
            if !written {
                self.show(MemoryPage::Index, cx);
            }
        }
        cx.notify();
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        if editor.busy {
            return;
        }
        let text = editor.text.read(cx).text().to_string();
        let path = match (&editor.path, &editor.name) {
            (Some(path), _) => path.clone(),
            (None, Some(name)) => {
                let typed = name.read(cx).text().to_string();
                let Some(path) = memory_path(&typed) else {
                    editor.error = Some("Name the file, like build-and-test.".into());
                    return cx.notify();
                };
                let taken = self
                    .view
                    .as_ref()
                    .is_some_and(|v| v.files.iter().any(|f| f.path == path));
                if taken {
                    editor.error = Some(format!("{path} already exists.").into());
                    return cx.notify();
                }
                path
            }
            (None, None) => return,
        };
        if text.len() > cypher_proto::MEMORY_FILE_MAX {
            editor.error = Some("Too long: a file keeps up to 64 KB.".into());
            return cx.notify();
        }
        let new = editor.path.is_none();
        editor.busy = true;
        editor.error = None;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            if let Some(editor) = self.editor.as_mut() {
                editor.busy = false;
                editor.error = Some("Engine not connected".into());
            }
            return cx.notify();
        };
        let params = serde_json::json!({
            "scope": self.scope,
            "edit": MemoryEdit::Write { path: path.clone(), text },
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::EDIT_MEMORY, params)
                .await
                .map_err(CallError::from);
            this.update(cx, |pane, cx| {
                match result {
                    Ok(_) => {
                        pane.editor = None;
                        if new {
                            pane.created = Some(path.clone());
                            pane.show(MemoryPage::File(path), cx);
                        }
                    }
                    Err(err) => {
                        if let Some(editor) = pane.editor.as_mut() {
                            editor.busy = false;
                            editor.error = Some(err.to_string().into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

// ── on the Botboard ─────────────────────────────────────────────────────────

/// The panel item for `page` of the store at `scope`.
pub(super) fn panel_item(scope: MemoryScope, page: MemoryPage) -> PanelItem {
    match page {
        MemoryPage::Index => PanelItem::Memory(scope),
        MemoryPage::File(path) => PanelItem::MemoryFile { scope, path },
        MemoryPage::New => PanelItem::NewMemoryFile(scope),
    }
}

impl Shell {
    /// The sidebar card: its header alone, saying how many files the
    /// workspace keeps or, in the amber of Needs you, how many writes wait
    /// on the owner.
    pub(super) fn render_memory_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let scope = MemoryScope::Workspace {
            bot_id: bot.id.clone(),
        };
        let store = self
            .state
            .read(cx)
            .bot_records
            .memory_store(&scope)
            .cloned();
        let (hint, color) = match store {
            Some(store) if store.pending > 0 => {
                (format!("{} to review", store.pending), Some(theme.warning))
            }
            Some(store) if store.files > 0 => (count(store.files as usize, "file", "files"), None),
            _ => ("No memory yet".to_string(), None),
        };
        Some(parts::header_card(
            "bot-memory-card",
            "Memory",
            hint,
            color,
            PanelItem::Memory(scope),
            self.card_focused("memory", cx),
            theme,
            cx,
        ))
    }

    /// The pane for `scope`, made (and watching) on first use; another
    /// store's pane goes.
    fn board_memory_pane(
        &mut self,
        scope: &MemoryScope,
        cx: &mut Context<Self>,
    ) -> Entity<MemoryPane> {
        if let Some((open, pane, _)) = &self.board.memory
            && open == scope
        {
            return pane.clone();
        }
        let state = self.state.clone();
        let pane = cx.new(|cx| MemoryPane::new(state, scope.clone(), cx));
        // Its pages draw in the panel, so a change in it redraws the shell.
        let subscriptions = vec![
            cx.observe(&pane, |_, _, cx| cx.notify()),
            cx.subscribe(
                &pane,
                |this: &mut Shell, pane, event: &MemoryPaneEvent, cx| {
                    let MemoryPaneEvent::Show(page) = event;
                    let scope = pane.read(cx).scope().clone();
                    this.show_panel(panel_item(scope, page.clone()), cx);
                },
            ),
        ];
        self.board.memory = Some((scope.clone(), pane.clone(), subscriptions));
        pane
    }

    /// A store's page in the detail panel: the workspace's own, or a
    /// project's it reads (reached from the project's page, and back).
    pub(super) fn render_memory_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let (scope, page) = match item {
            PanelItem::Memory(scope) => (scope, MemoryPage::Index),
            PanelItem::MemoryFile { scope, path } => (scope, MemoryPage::File(path.clone())),
            PanelItem::NewMemoryFile(scope) => (scope, MemoryPage::New),
            _ => return None,
        };
        let project = match scope {
            MemoryScope::Workspace { bot_id } if *bot_id == bot.id => None,
            MemoryScope::Project { space_id } if bot.scope.sees_space(space_id) => Some(
                self.state
                    .read(cx)
                    .space_row(space_id)?
                    .display_name()
                    .to_string(),
            ),
            _ => return None,
        };
        let pane = self.board_memory_pane(scope, cx);
        let parts = pane.update(cx, |pane, cx| pane.render_page(&page, window, cx));
        self.set_panel_actions(parts.actions);
        self.board.panel_footer = parts.footer;
        self.board.panel_empty = parts.empty;
        let title = match &page {
            MemoryPage::Index => {
                // A project's store names its project after the title and
                // goes back to its page.
                if let (Some(name), MemoryScope::Project { space_id }) = (&project, scope) {
                    self.board.panel_title_note = Some(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(name.clone()))
                            .into_any_element(),
                    );
                    self.set_panel_back(
                        "bot-memory-back",
                        "Back to the project",
                        PanelItem::Project(space_id.clone()),
                        cx,
                    );
                }
                "Memory".to_string()
            }
            MemoryPage::File(path) => {
                self.set_panel_back(
                    "bot-memory-back",
                    "Back to memory",
                    PanelItem::Memory(scope.clone()),
                    cx,
                );
                file_name(path)
            }
            MemoryPage::New => {
                self.set_panel_back(
                    "bot-memory-back",
                    "Back to memory",
                    PanelItem::Memory(scope.clone()),
                    cx,
                );
                "New file".to_string()
            }
        };
        let head = PanelHead {
            glyph: icons::BOOK,
            tint: None,
            title,
        };
        Some((head, parts.body))
    }
}

// ── pieces ──────────────────────────────────────────────────────────────────

/// A small square button for a section heading (Edit beside MEMORY.md).
fn small_button(
    key: &'static str,
    glyph: &'static str,
    tip: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(key)
        .flex_none()
        .size(px(22.0))
        .mr(px(-5.0))
        .my(px(-4.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(motion::hover_blend(
            key,
            crate::kit::theme::wash(0.0),
            crate::kit::theme::wash(0.12),
        ))
        .on_hover(motion::hover_listener(key))
        .tooltip(move |_, cx| {
            cx.new(|_| super::super::session::FindTooltip(tip.into()))
                .into()
        })
        .child(icon(glyph).size(px(12.0)).text_color(motion::hover_blend(
            key,
            theme.text_muted,
            theme.text,
        )))
}

/// A row's title over its one-line detail, with an optional tinted tail on
/// the detail line ("edited your text").
fn two_line(
    title: String,
    detail: String,
    tail: Option<(String, gpui::Hsla)>,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(13.0))
                .line_height(px(18.0))
                .text_color(theme.text)
                .child(SharedString::from(title)),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_row()
                .gap(px(4.0))
                .text_size(px(11.5))
                .line_height(px(16.0))
                .text_color(theme.text_muted.opacity(0.85))
                .child(div().min_w_0().truncate().child(SharedString::from(detail)))
                .when_some(tail, |el, (tail, color)| {
                    el.child(
                        div()
                            .flex_none()
                            .text_color(color)
                            .child(SharedString::from(format!("· {tail}"))),
                    )
                }),
        )
}

/// "You · 2h ago": who last changed a file, and when.
fn by_line(file: &MemoryFile) -> String {
    format!(
        "{} · {}",
        file.author.label(),
        parts::ago_ms(file.updated_at.max(file.created_at))
    )
}

/// A path's last part: `/decisions/rounding.md` reads `rounding.md`.
pub(crate) fn file_name(path: &str) -> String {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// What a name typed for a new file is as a store path: `testing`,
/// `/testing.md` and `testing.MD` all read `/testing.md`. `None` when it
/// can't be one ([`cypher_proto::valid_memory_path`]).
pub(super) fn memory_path(typed: &str) -> Option<String> {
    let name = typed.trim().trim_start_matches('/');
    if name.is_empty() {
        return None;
    }
    let stem = match name.len().checked_sub(3) {
        Some(at) if name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(".md") => {
            &name[..at]
        }
        _ => name,
    };
    let path = format!("/{stem}.md");
    cypher_proto::valid_memory_path(&path).then_some(path)
}

/// The hour a consolidation field names: `3`, `03`, `3:00` and `03:00` are
/// 3; minutes other than :00 are not (a pass starts on the hour).
pub(super) fn parse_hour(text: &str) -> Option<u8> {
    let text = text.trim();
    let (hour, minutes) = text.split_once(':').unwrap_or((text, "00"));
    let digits = |s: &str, max: usize| {
        !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(hour, 2) || !digits(minutes, 2) || minutes.parse::<u8>().ok() != Some(0) {
        return None;
    }
    hour.parse::<u8>().ok().filter(|h| *h < 24)
}

fn hour_label(hour: u8) -> String {
    format!("{hour:02}:00")
}

/// "820 B", "1.2 KB", "64 KB".
pub(super) fn size_label(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        _ if bytes < 10 * 1024 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{} KB", bytes / 1024),
    }
}

/// "820 tokens", "12.4k tokens", "1.2M tokens".
pub(super) fn tokens_label(tokens: u64) -> String {
    match tokens {
        0..1000 => format!("{tokens} tokens"),
        1000..1_000_000 => format!("{:.1}k tokens", tokens as f64 / 1000.0),
        _ => format!("{:.1}M tokens", tokens as f64 / 1_000_000.0),
    }
}

/// What a pass changed and cost: "3 changes · 12.4k tokens".
fn pass_cost(changes: u32, tokens: u64) -> Option<String> {
    let parts: Vec<String> = [
        (changes > 0).then(|| count(changes as usize, "change", "changes")),
        (tokens > 0).then(|| tokens_label(tokens)),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A pass runs, or Run now asked for one its host has not started yet.
pub(super) fn pass_waiting(store: &MemoryStore) -> bool {
    let last = store.last_pass.as_ref();
    last.is_some_and(|p| p.is_running())
        || store
            .pass_requested_at
            .is_some_and(|at| last.is_none_or(|p| p.at < at))
}

/// Whether a change can still be put back: it kept the version before it,
/// is not undone, and is not itself an undo.
pub(super) fn undoable(change: &MemoryChange) -> bool {
    change.undo.is_some() && !change.undone && !matches!(change.action, MemoryAction::Undo { .. })
}

/// A change in a line: its own summary, else what it did, on which file.
pub(super) fn change_title(change: &MemoryChange) -> String {
    let file = file_name(&change.path);
    let summary = change.summary.trim();
    if !summary.is_empty() {
        return format!("{file} · {}", transcript::single_line(summary));
    }
    match &change.action {
        MemoryAction::Create => format!("Created {file}"),
        MemoryAction::Edit => format!("Edited {file}"),
        MemoryAction::Delete => format!("Deleted {file}"),
        MemoryAction::Rename { from } => format!("Renamed {} to {file}", file_name(from)),
        MemoryAction::Undo { .. } => format!("Put back {file}"),
    }
}

fn action_glyph(action: &MemoryAction) -> &'static str {
    match action {
        MemoryAction::Create => icons::DOCUMENT_ADD,
        MemoryAction::Edit => icons::PEN,
        MemoryAction::Delete => icons::TRASH_BIN_MINIMALISTIC,
        MemoryAction::Rename { .. } => icons::ARROW_RIGHT,
        MemoryAction::Undo { .. } => icons::RESTART,
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::MemoryAuthor;

    fn change(action: MemoryAction, summary: &str) -> MemoryChange {
        MemoryChange {
            id: "c".into(),
            at: 0,
            author: MemoryAuthor::Owner,
            path: "/decisions/rounding.md".into(),
            action,
            summary: summary.into(),
            before: None,
            owner_text: false,
            undo: Some("ab".into()),
            undone: false,
        }
    }

    #[test]
    fn new_file_names_become_store_paths() {
        assert_eq!(memory_path("testing").as_deref(), Some("/testing.md"));
        assert_eq!(memory_path(" /testing.md ").as_deref(), Some("/testing.md"));
        assert_eq!(memory_path("Build.MD").as_deref(), Some("/Build.md"));
        assert_eq!(
            memory_path("decisions/rounding").as_deref(),
            Some("/decisions/rounding.md")
        );
        for bad in ["", "  ", "/", "../x", ".hidden", "a//b", "a\\b"] {
            assert_eq!(memory_path(bad), None, "{bad:?}");
        }
        assert_eq!(file_name("/decisions/rounding.md"), "rounding.md");
        assert_eq!(file_name(MEMORY_INDEX), "MEMORY.md");
    }

    #[test]
    fn hours_read_on_the_hour() {
        for (text, hour) in [
            ("3", 3),
            ("03", 3),
            ("3:00", 3),
            ("03:00", 3),
            (" 23 ", 23),
            ("0", 0),
        ] {
            assert_eq!(parse_hour(text), Some(hour), "{text}");
        }
        for bad in ["", "24", "3:30", "3:", "abc", "003", "-1", "12:000"] {
            assert_eq!(parse_hour(bad), None, "{bad}");
        }
        assert_eq!(hour_label(3), "03:00");
    }

    #[test]
    fn sizes_and_costs_read_short() {
        assert_eq!(size_label(820), "820 B");
        assert_eq!(size_label(1229), "1.2 KB");
        assert_eq!(size_label(64 * 1024), "64 KB");
        assert_eq!(tokens_label(820), "820 tokens");
        assert_eq!(tokens_label(12_400), "12.4k tokens");
        assert_eq!(tokens_label(1_250_000), "1.2M tokens");
        assert_eq!(
            pass_cost(3, 12_400).as_deref(),
            Some("3 changes · 12.4k tokens")
        );
        assert_eq!(pass_cost(1, 0).as_deref(), Some("1 change"));
        assert_eq!(pass_cost(0, 0), None);
    }

    #[test]
    fn a_pass_waits_while_it_runs_or_was_asked_for() {
        let mut store = MemoryStore::new("m".into(), MemoryScope::You, "d".into(), 0);
        assert!(!pass_waiting(&store));
        store.pass_requested_at = Some(10);
        assert!(pass_waiting(&store), "asked for, none ran yet");
        store.last_pass = Some(cypher_proto::MemoryPass {
            at: 20,
            outcome: PassOutcome::Done,
            ..Default::default()
        });
        assert!(!pass_waiting(&store), "the pass it asked for ran");
        store.last_pass.as_mut().unwrap().outcome = PassOutcome::Running;
        assert!(pass_waiting(&store));
    }

    #[test]
    fn changes_say_what_they_did_and_whether_they_undo() {
        assert_eq!(
            change_title(&change(MemoryAction::Edit, "")),
            "Edited rounding.md"
        );
        assert_eq!(
            change_title(&change(MemoryAction::Edit, "Added 2 lines")),
            "rounding.md · Added 2 lines"
        );
        assert_eq!(
            change_title(&change(
                MemoryAction::Rename {
                    from: "/round.md".into()
                },
                ""
            )),
            "Renamed round.md to rounding.md"
        );
        assert!(undoable(&change(MemoryAction::Edit, "")));
        let mut undone = change(MemoryAction::Edit, "");
        undone.undone = true;
        assert!(!undoable(&undone));
        let mut lost = change(MemoryAction::Delete, "");
        lost.undo = None;
        assert!(!undoable(&lost));
        assert!(!undoable(&change(
            MemoryAction::Undo {
                change_id: "x".into()
            },
            ""
        )));
    }
}
