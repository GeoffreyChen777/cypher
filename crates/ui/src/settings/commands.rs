//! Settings → Commands: which Pi slash commands the composer `/` menu shows.
//!
//! Cypher is a GUI app, so the menu starts with only
//! [`SHOWN_BY_DEFAULT`](crate::prefs::slash_commands::SHOWN_BY_DEFAULT): any
//! other command appears once the user turns it on here, and that includes
//! commands a newly installed extension adds later. Discovery is the harness's full `ListCommands` list;
//! the page groups it by purpose ([`placement`]) and keeps commands that only
//! configure what a Settings page already covers behind each group's
//! collapsed "Advanced" row. The shown names are a device-local preference in
//! `ui-settings.json`; a hidden command still runs if typed.

use std::collections::HashSet;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, Render, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};

use cypher_proto::{HarnessId, SlashCommand};
use cypher_rpc::methods;

use super::device_target::DeviceTarget;
use crate::kit::icons;
use crate::kit::popover::{self, Loadable};
use crate::kit::theme::Theme;
use crate::prefs::slash_commands::{CommandGroup, placement, set_visible, shows};
use crate::settings::widgets;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub enum CommandsEvent {
    /// The shown-name list changed — persist and publish.
    Changed(Vec<String>),
}

pub struct CommandsPage {
    state: Entity<AppState>,
    target: Entity<DeviceTarget>,
    generation: u64,
    _target_observer: Subscription,
    _catalog_observer: Subscription,
    commands: Loadable<Vec<SlashCommand>>,
    shown: Vec<String>,
    /// Groups whose "Advanced" row the user expanded (page-local).
    advanced_open: HashSet<CommandGroup>,
    load_task: Option<Task<()>>,
}

impl EventEmitter<CommandsEvent> for CommandsPage {}

impl CommandsPage {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        shown: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let generation = target.read(cx).generation();
        let observer = cx.observe(&target, |page: &mut Self, target, cx| {
            let generation = target.read(cx).generation();
            if generation != page.generation {
                page.generation = generation;
                page.load_task = None;
                page.commands = Loadable::Idle;
                page.load(cx);
            }
            cx.notify();
        });
        let catalog_observer =
            cx.observe_global::<crate::pickers::HarnessCatalogChanged>(|page: &mut Self, cx| {
                page.load(cx)
            });
        let mut page = Self {
            state,
            target,
            generation,
            _target_observer: observer,
            _catalog_observer: catalog_observer,
            commands: Loadable::Idle,
            shown,
            advanced_open: HashSet::new(),
            load_task: None,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let ticket = match self.target.read(cx).ticket(cx) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.commands = Loadable::Error(error);
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.commands = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::LIST_COMMANDS,
                    ticket.params(serde_json::json!({ "harness": HarnessId::Pi })),
                )
                .await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.commands = match result {
                    Ok(value) => match serde_json::from_value::<Vec<SlashCommand>>(value) {
                        Ok(commands) => Loadable::Ready(commands),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(format!("{}: {err}", ticket.label)),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_visible(&mut self, names: Vec<String>, visible: bool, cx: &mut Context<Self>) {
        self.shown = set_visible(&self.shown, &names, visible);
        cx.emit(CommandsEvent::Changed(self.shown.clone()));
        cx.notify();
    }

    /// One group's card: a header (title, caption, how many of its main
    /// commands are shown, and a switch for all of them), the main commands,
    /// then the collapsed "Advanced" row and, when open, its commands one
    /// level in.
    fn group_card(
        &self,
        group: CommandGroup,
        members: &[&SlashCommand],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (advanced, main): (Vec<&SlashCommand>, Vec<&SlashCommand>) = members
            .iter()
            .copied()
            .partition(|command| placement(&command.name).advanced);
        let shown_count = |commands: &[&SlashCommand]| {
            commands
                .iter()
                .filter(|command| shows(&self.shown, &command.name))
                .count()
        };
        let main_names: Vec<String> = main.iter().map(|command| command.name.clone()).collect();
        let main_shown = shown_count(&main);
        let all_on = !main.is_empty() && main_shown == main.len();
        let header = div()
            .px(px(20.0))
            .py(px(14.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(group.title())),
                    )
                    .child(
                        div()
                            .mt(px(3.0))
                            .text_size(px(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(group.caption())),
                    ),
            )
            // A group of configuration only has no main layer to switch.
            .when(!main.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted.opacity(0.7))
                        .child(SharedString::from(format!(
                            "{main_shown} of {} shown",
                            main.len()
                        ))),
                )
                .child(
                    widgets::toggle_switch(theme, all_on)
                        .id(SharedString::from(format!("slash-group-{group:?}")))
                        .cursor_pointer()
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.set_visible(main_names.clone(), !all_on, cx);
                        })),
                )
            });
        let mut card = widgets::section_card(theme).child(header).children(
            main.iter()
                .map(|command| self.command_row(command, false, theme, cx)),
        );
        if !advanced.is_empty() {
            let open = self.advanced_open.contains(&group);
            let advanced_shown = shown_count(&advanced);
            card = card.child(
                div()
                    .id(SharedString::from(format!("slash-advanced-{group:?}")))
                    .border_t_1()
                    .border_color(theme.border)
                    .px(px(20.0))
                    .py(px(10.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(crate::kit::theme::ink(0.015)))
                    .on_click(cx.listener(move |page, _, _, cx| {
                        if !page.advanced_open.remove(&group) {
                            page.advanced_open.insert(group);
                        }
                        cx.notify();
                    }))
                    .child(
                        icons::icon(if open {
                            icons::ALT_ARROW_DOWN
                        } else {
                            icons::ALT_ARROW_RIGHT
                        })
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child(SharedString::from("Advanced")),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_muted.opacity(0.7))
                            .child(SharedString::from(format!(
                                "{advanced_shown} of {} shown",
                                advanced.len()
                            ))),
                    ),
            );
            if open {
                card = card.children(
                    advanced
                        .iter()
                        .map(|command| self.command_row(command, true, theme, cx)),
                );
            }
        }
        card.into_any_element()
    }

    /// One command: `/name`, its description, and its own switch. `nested`
    /// rows sit under the "Advanced" row, their text aligned with its label.
    fn command_row(
        &self,
        command: &SlashCommand,
        nested: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = command.name.clone();
        let shown = shows(&self.shown, &name);
        div()
            .border_t_1()
            .border_color(theme.border)
            // Advanced row: 20px inset + 14px chevron + 8px gap.
            .pl(px(if nested { 42.0 } else { 20.0 }))
            .pr(px(20.0))
            .py(px(10.0))
            .hover(|s| s.bg(crate::kit::theme::ink(0.015)))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, format!("/{name}")))
                    .when(!command.description.is_empty(), |el| {
                        el.child(
                            div()
                                .mt(px(3.0))
                                .w_full()
                                .min_w_0()
                                .overflow_hidden()
                                .truncate()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted.opacity(0.65))
                                .child(SharedString::from(command.description.clone())),
                        )
                    }),
            )
            .child(
                widgets::toggle_switch(theme, shown)
                    .id(SharedString::from(format!("slash-command-{name}")))
                    .cursor_pointer()
                    .on_click(cx.listener(move |page, _, _, cx| {
                        page.set_visible(vec![name.clone()], !shown, cx);
                    })),
            )
            .into_any_element()
    }
}

impl Render for CommandsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let body: AnyElement = match self.commands.clone() {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "commands-skeleton",
                    &theme,
                    6,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Loadable::Error(message) => div()
                .child(widgets::error_strip(&theme, message))
                .child(
                    widgets::ghost_action(&theme)
                        .id("commands-retry")
                        .mt(px(8.0))
                        .text_color(theme.text)
                        .hover(|s| widgets::ghost_hover(&theme, s))
                        .on_click(cx.listener(|page, _, _, cx| page.load(cx)))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element(),
            Loadable::Ready(commands) if commands.is_empty() => widgets::section_card(&theme)
                .p(px(16.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(
                            "No slash commands yet. Install Pi and extensions in Agents.",
                        )),
                )
                .into_any_element(),
            Loadable::Ready(commands) => div()
                .flex()
                .flex_col()
                .children(CommandGroup::ALL.into_iter().filter_map(|group| {
                    let members: Vec<&SlashCommand> = commands
                        .iter()
                        .filter(|command| placement(&command.name).group == group)
                        .collect();
                    (!members.is_empty()).then(|| self.group_card(group, &members, &theme, cx))
                }))
                .into_any_element(),
        };

        let visible_count = match &self.commands {
            Loadable::Ready(commands) => Some(
                commands
                    .iter()
                    .filter(|command| shows(&self.shown, &command.name))
                    .count(),
            ),
            _ => None,
        };

        div()
            .id("commands-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(&theme, "Commands", visible_count))
                    .child(
                        widgets::page_subtitle(
                            &theme,
                            "The composer's / menu shows only the commands you turn on here. A hidden command still runs if you type it. Saved on this client only.",
                        )
                        .max_w(px(560.0))
                        .line_height(px(20.0)),
                    )
                    .when_some(self.target.read(cx).unavailable(cx), |el, error|
                        el.child(widgets::warning_strip(&theme, error)))
                    .child(body),
            )
    }
}
