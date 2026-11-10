//! Settings → Memory (docs/design/memory.md §8): what agents keep between
//! sessions, and the one place to read and change it. Yours first — read by
//! every session and workspace — then each project's, kept by the sessions
//! in it, then each workspace's own. A store opens on the page
//! [`crate::shell::MemoryPane`] draws; `/memory` in a session lands here on
//! its project's store. Writes held for review ask in the chat that made
//! them ([`crate::transcript`]) and wait on the store's page too.

use gpui::{AnyElement, Context, Entity, SharedString, Subscription, Window, div, prelude::*, px};

use cypher_proto::{MemoryScope, MemoryStore};

use crate::kit::icons::{self, icon};
use crate::kit::theme::Theme;
use crate::settings::widgets;
use crate::shell::{MemoryPage, MemoryPane, MemoryPaneEvent};
use crate::state::AppState;

pub struct MemorySettingsPage {
    state: Entity<AppState>,
    pane: Entity<MemoryPane>,
    page: MemoryPage,
    _pane_events: Vec<Subscription>,
    _observe: Subscription,
}

/// A store on one of the page's lists.
struct Listed {
    name: String,
    glyph: &'static str,
    store: MemoryStore,
}

/// The stores the page lists, each by name: the projects', then the
/// workspaces'. A store whose project or workspace is gone is left out.
fn listed_stores(state: &AppState) -> (Vec<Listed>, Vec<Listed>) {
    let (mut projects, mut workspaces) = (Vec::new(), Vec::new());
    for store in &state.bot_records.memory_stores {
        match &store.scope {
            MemoryScope::Project { space_id } => {
                if let Some(space) = state.space_row(space_id) {
                    projects.push(Listed {
                        name: space.display_name().to_string(),
                        glyph: crate::appearance::space_style::space_icon(space.icon.as_deref()),
                        store: store.clone(),
                    });
                }
            }
            MemoryScope::Workspace { bot_id } => {
                if let Some(bot) = state.bots.iter().find(|b| b.id == *bot_id) {
                    workspaces.push(Listed {
                        name: bot.name.clone(),
                        glyph: crate::appearance::bot_style::bot_icon(bot.icon.as_deref())
                            .unwrap_or(icons::BOT),
                        store: store.clone(),
                    });
                }
            }
            MemoryScope::You | MemoryScope::Role { .. } => {}
        }
    }
    for list in [&mut projects, &mut workspaces] {
        list.sort_by_key(|listed| listed.name.to_lowercase());
    }
    (projects, workspaces)
}

impl MemorySettingsPage {
    pub fn new(state: Entity<AppState>, scope: MemoryScope, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        let (pane, events) = Self::make_pane(&state, scope, cx);
        Self {
            state,
            pane,
            page: MemoryPage::Index,
            _pane_events: events,
            _observe: observe,
        }
    }

    fn make_pane(
        state: &Entity<AppState>,
        scope: MemoryScope,
        cx: &mut Context<Self>,
    ) -> (Entity<MemoryPane>, Vec<Subscription>) {
        let state = state.clone();
        let pane = cx.new(|cx| MemoryPane::new(state, scope, cx));
        // Its pages draw in this one, so a change in it redraws the page.
        let events = vec![
            cx.observe(&pane, |_, _, cx| cx.notify()),
            cx.subscribe(&pane, |page: &mut Self, _, event: &MemoryPaneEvent, cx| {
                let MemoryPaneEvent::Show(shown) = event;
                page.page = shown.clone();
                cx.notify();
            }),
        ];
        (pane, events)
    }

    /// Show `scope`'s store from its top.
    pub fn open(&mut self, scope: MemoryScope, cx: &mut Context<Self>) {
        if *self.pane.read(cx).scope() != scope {
            let (pane, events) = Self::make_pane(&self.state, scope, cx);
            self.pane = pane;
            self._pane_events = events;
        }
        self.page = MemoryPage::Index;
        cx.notify();
    }

    /// A project's or workspace's name, for its store's title.
    fn store_name(&self, scope: &MemoryScope, cx: &Context<Self>) -> Option<String> {
        let state = self.state.read(cx);
        match scope {
            MemoryScope::Project { space_id } => Some(
                state
                    .space_row(space_id)
                    .map_or("A removed project", |s| s.display_name())
                    .to_string(),
            ),
            MemoryScope::Workspace { bot_id } => Some(
                state
                    .bots
                    .iter()
                    .find(|b| b.id == *bot_id)
                    .map_or("A removed workspace", |b| b.name.as_str())
                    .to_string(),
            ),
            MemoryScope::You | MemoryScope::Role { .. } => None,
        }
    }

    /// One list's stores, a row each; a click opens it. An empty list
    /// says `empty`.
    fn stores_card(
        &self,
        key: &'static str,
        stores: Vec<Listed>,
        empty: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if stores.is_empty() {
            return widgets::section_card(theme)
                .mt(px(8.0))
                .px(px(20.0))
                .py(px(16.0))
                .text_size(px(13.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(empty))
                .into_any_element();
        }
        let now = chrono::Utc::now();
        let rows = stores
            .into_iter()
            .enumerate()
            .map(|(ix, Listed { name, glyph, store })| {
                let mut facts: Vec<AnyElement> = vec![
                    div()
                        .child(SharedString::from(match store.files {
                            1 => "1 file".to_string(),
                            n => format!("{n} files"),
                        }))
                        .into_any_element(),
                ];
                if store.pending > 0 {
                    facts.push(
                        div()
                            .text_color(theme.warning)
                            .child(SharedString::from(format!("{} to review", store.pending)))
                            .into_any_element(),
                    );
                }
                if let Some(at) = chrono::DateTime::from_timestamp_millis(store.updated_at) {
                    facts.push(
                        div()
                            .child(SharedString::from(format!(
                                "updated {}",
                                crate::state::format_time_ago(at, now)
                            )))
                            .into_any_element(),
                    );
                }
                let scope = store.scope.clone();
                widgets::card_row(theme, ix == 0)
                    .id((key, ix))
                    .cursor_pointer()
                    .on_click(cx.listener(move |page, _, _, cx| page.open(scope.clone(), cx)))
                    .child(widgets::row_tile(theme, glyph))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(theme, name))
                            .child(widgets::meta_line(theme, facts)),
                    )
                    .child(
                        icon(icons::ALT_ARROW_RIGHT)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    )
            });
        widgets::section_card(theme)
            .mt(px(8.0))
            .children(rows)
            .into_any_element()
    }
}

/// A heading over one of the page's cards: its label, a quiet note, and
/// whatever acts on the card at its end.
fn card_heading(theme: &Theme, label: &str, note: &str, actions: Vec<AnyElement>) -> gpui::Div {
    div()
        .mt(px(24.0))
        .min_h(px(28.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(widgets::field_label(theme, label.to_string()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(12.0))
                .text_color(theme.text_muted.opacity(0.8))
                .child(SharedString::from(note.to_string())),
        )
        .children(actions)
}

impl Render for MemorySettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let scope = self.pane.read(cx).scope().clone();
        let page = self.page.clone();
        let parts = self
            .pane
            .update(cx, |pane, cx| pane.render_page(&page, window, cx));
        let named = self.store_name(&scope, cx);
        let store_title = named.clone().unwrap_or_else(|| "Memory".to_string());

        // Where the page sits: the store's top (yours is the page's own),
        // or a file of it, one step back from either.
        let root = scope == MemoryScope::You && page == MemoryPage::Index;
        let (title, back): (String, Option<(String, MemoryScope)>) = match &page {
            MemoryPage::Index if root => ("Memory".into(), None),
            MemoryPage::Index => (
                store_title.clone(),
                Some(("Memory".into(), MemoryScope::You)),
            ),
            MemoryPage::File(path) => (
                crate::shell::memory_file_name(path),
                Some((store_title.clone(), scope.clone())),
            ),
            MemoryPage::New => (
                "New file".into(),
                Some((store_title.clone(), scope.clone())),
            ),
        };
        let back = back.map(|(label, to)| {
            div()
                .id("memory-settings-back")
                .mb(px(10.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(theme.text_muted)
                .hover(|s| s.text_color(theme.text))
                .on_click(cx.listener(move |page, _, _, cx| page.open(to.clone(), cx)))
                .child(
                    icon(icons::ALT_ARROW_LEFT)
                        .size(px(11.0))
                        .text_color(theme.text_muted),
                )
                .child(SharedString::from(label))
        });
        let body = match parts.empty {
            Some(empty) => div()
                .py(px(40.0))
                .px(px(24.0))
                .flex()
                .justify_center()
                .child(empty),
            None => parts.body,
        };
        let card = widgets::section_card(&theme)
            .mt(px(if root { 8.0 } else { 20.0 }))
            .p(px(8.0))
            .child(body);
        let footer = parts.footer.map(|footer| div().mt(px(12.0)).child(footer));

        let mut column = widgets::page_column().children(back);
        if root {
            let (projects, workspaces) = listed_stores(self.state.read(cx));
            column = column
                .child(widgets::page_header(&theme, "Memory", None))
                .child(widgets::page_subtitle(
                    &theme,
                    "What agents learn from working with you and keep between sessions, as \
                     files you can read and edit.",
                ))
                .child(card_heading(
                    &theme,
                    "Yours",
                    "Every session and workspace reads it",
                    parts.actions,
                ))
                .child(card)
                .children(footer)
                .child(card_heading(
                    &theme,
                    "Projects",
                    "Each read by the sessions in its project",
                    Vec::new(),
                ))
                .child(self.stores_card(
                    "memory-project",
                    projects,
                    "None yet. Sessions keep what they learn about their project here.",
                    &theme,
                    cx,
                ));
            if !workspaces.is_empty() {
                column = column
                    .child(card_heading(
                        &theme,
                        "Workspaces",
                        "Each kept and read by its workspace",
                        Vec::new(),
                    ))
                    .child(self.stores_card("memory-workspace", workspaces, "", &theme, cx));
            }
        } else {
            column = column
                .child(
                    widgets::page_header(&theme, &title, None)
                        .items_center()
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(2.0))
                                .children(parts.actions),
                        ),
                )
                .when_some(
                    match (&page, &scope) {
                        (MemoryPage::Index, MemoryScope::Project { .. }) => Some(
                            "What sessions in this project learned. Every one of them reads \
                             it, and so do the workspaces that read the project.",
                        ),
                        (MemoryPage::Index, MemoryScope::Workspace { .. }) => Some(
                            "What this workspace learned coordinating your work. Its chat \
                             and sessions read it; your other sessions don't.",
                        ),
                        _ => None,
                    },
                    |el, subtitle| el.child(widgets::page_subtitle(&theme, subtitle)),
                )
                .child(card)
                .children(footer);
        }
        div()
            .id("memory-settings-page")
            .size_full()
            .overflow_y_scroll()
            .child(column)
    }
}
