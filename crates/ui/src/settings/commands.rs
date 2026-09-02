//! Settings → Commands: which Pi slash commands the composer `/` menu shows.
//!
//! Discovery is the harness's full `ListCommands` list. Hide/show is a
//! device-local preference in `ui-settings.json`; hidden names still run if
//! typed. Until the user customizes, [`DEFAULT_HIDDEN`] applies.

use gpui::{
    App, Context, Entity, EventEmitter, Global, Render, SharedString, Task, Window, div, prelude::*,
    px,
};

use cypher_proto::{HarnessId, SlashCommand};
use cypher_rpc::methods;

use crate::icons;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// Default hide rules until the user customizes Settings → Commands:
/// skills, llama, NewAPI providers, compact-ui, and MCP commands.
pub fn default_hides(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("skill:")
        || name.starts_with("llama")
        || name.starts_with("newapi-")
        || name.starts_with("compact-ui")
        || name == "mcp"
        || name.starts_with("mcp-")
        || name.starts_with("pi-mcp")
}

/// `None` = apply [`default_hides`]; `Some` = exact names the user hid.
#[derive(Clone)]
pub struct HiddenSlashCommands {
    pub stored: Option<Vec<String>>,
}

impl Global for HiddenSlashCommands {}

pub fn hides(stored: Option<&[String]>, name: &str) -> bool {
    match stored {
        Some(list) => list.iter().any(|item| item == name),
        None => default_hides(name),
    }
}

pub fn materialize_hidden(
    commands: &[SlashCommand],
    stored: Option<&[String]>,
) -> Vec<String> {
    match stored {
        Some(list) => list.to_vec(),
        None => commands
            .iter()
            .filter(|command| default_hides(&command.name))
            .map(|command| command.name.clone())
            .collect(),
    }
}

pub fn set_visible(
    commands: &[SlashCommand],
    stored: Option<&[String]>,
    name: &str,
    visible: bool,
) -> Vec<String> {
    let mut list = materialize_hidden(commands, stored);
    if visible {
        list.retain(|item| item != name);
    } else if !list.iter().any(|item| item == name) {
        list.push(name.to_string());
    }
    list
}

pub fn publish_hidden(stored: Option<Vec<String>>, cx: &mut App) {
    cx.set_global(HiddenSlashCommands { stored });
}

pub fn stored_from_app(cx: &App) -> Option<Vec<String>> {
    cx.try_global::<HiddenSlashCommands>()
        .and_then(|slot| slot.stored.clone())
}

pub fn hides_in_app(cx: &App, name: &str) -> bool {
    hides(stored_from_app(cx).as_deref(), name)
}

#[derive(Debug, Clone)]
pub enum CommandsEvent {
    /// The hidden-name list changed — persist and publish.
    Changed(Vec<String>),
}

pub struct CommandsPage {
    state: Entity<AppState>,
    commands: Loadable<Vec<SlashCommand>>,
    /// `None` = default hide rules; `Some` = user-customized exact names.
    hidden: Option<Vec<String>>,
    load_task: Option<Task<()>>,
}

impl EventEmitter<CommandsEvent> for CommandsPage {}

impl CommandsPage {
    pub fn new(
        state: Entity<AppState>,
        hidden: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut page = Self {
            state,
            commands: Loadable::Idle,
            hidden,
            load_task: None,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.commands = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::LIST_COMMANDS,
                    serde_json::json!({ "harness": HarnessId::Pi }),
                )
                .await;
            this.update(cx, |page, cx| {
                page.commands = match result {
                    Ok(value) => match serde_json::from_value::<Vec<SlashCommand>>(value) {
                        Ok(commands) => Loadable::Ready(commands),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_visible(&mut self, name: String, visible: bool, cx: &mut Context<Self>) {
        let commands = self.commands.ready().cloned().unwrap_or_default();
        let hidden = set_visible(&commands, self.hidden.as_deref(), &name, visible);
        self.hidden = Some(hidden.clone());
        cx.emit(CommandsEvent::Changed(hidden));
        cx.notify();
    }
}

impl Render for CommandsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let stored = self.hidden.clone();
        let body: gpui::AnyElement = match self.commands.clone() {
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
            Loadable::Ready(commands) => widgets::section_card(&theme)
                .children(commands.into_iter().enumerate().map(|(index, command)| {
                    let name = command.name.clone();
                    let shown = !hides(stored.as_deref(), &name);
                    let title = format!("/{name}");
                    widgets::card_row(&theme, index == 0)
                        .id(("slash-command-row", index))
                        .child(widgets::row_tile(&theme, icons::COMMAND))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(widgets::row_title(&theme, title))
                                .when(!command.description.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .mt(px(4.0))
                                            .w_full()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .truncate()
                                            .text_size(px(11.5))
                                            .text_color(theme.text_muted.opacity(0.65))
                                            .child(SharedString::from(command.description)),
                                    )
                                }),
                        )
                        .child(
                            widgets::toggle_switch(&theme, shown)
                                .flex_none()
                                .id(("slash-command-toggle", index))
                                .cursor_pointer()
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    page.set_visible(name.clone(), !shown, cx);
                                })),
                        )
                        .into_any_element()
                }))
                .into_any_element(),
        };

        let visible_count = match &self.commands {
            Loadable::Ready(commands) => Some(
                commands
                    .iter()
                    .filter(|command| !hides(self.hidden.as_deref(), &command.name))
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
                            "Choose which slash commands appear when you type /. Hidden commands still run if you type them.",
                        )
                        .max_w(px(560.0))
                        .line_height(px(20.0)),
                    )
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: String::new(),
            input_hint: None,
        }
    }

    #[test]
    fn defaults_hide_skills_llama_newapi_compact_ui_and_mcp() {
        for name in [
            "skill:brave-search",
            "llama",
            "llama-cpp",
            "newapi-provider-add",
            "newapi-provider-list",
            "newapi-generate-models-json",
            "compact-ui-config",
            "mcp",
            "mcp-auth",
            "pi-mcp",
        ] {
            assert!(default_hides(name), "{name}");
            assert!(hides(None, name), "{name}");
        }
        for name in ["goal", "fast", "subagent-config", "compact", "perm-mode"] {
            assert!(!default_hides(name), "{name}");
        }
    }

    #[test]
    fn empty_store_shows_all() {
        assert!(!hides(Some(&[]), "mcp"));
        assert!(!hides(Some(&[]), "skill:brave-search"));
    }

    #[test]
    fn toggling_materializes_defaults_then_persists() {
        let commands = vec![cmd("goal"), cmd("mcp"), cmd("skill:x"), cmd("compact")];
        let shown = set_visible(&commands, None, "mcp", true);
        assert!(!hides(Some(&shown), "mcp"));
        assert!(hides(Some(&shown), "skill:x"));
        assert!(!hides(Some(&shown), "goal"));
        let hidden = set_visible(&commands, Some(&shown), "goal", false);
        assert!(hides(Some(&hidden), "goal"));
    }
}
