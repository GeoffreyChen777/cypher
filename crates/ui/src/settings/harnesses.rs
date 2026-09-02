//! Settings → Agents: Pi installation and Pi package management.

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, SharedString, Task, Window, div, prelude::*,
    px,
};

use cypher_engine::pi_packages::{PiPackage, PiPackagesSnapshot};
use cypher_rpc::methods;

use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct HarnessesPage {
    state: Entity<AppState>,
    packages: Loadable<PiPackagesSnapshot>,
    target_device: Option<String>,
    device_menu_open: bool,
    device_menu_pressed_open: bool,
    error: Option<String>,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
}

impl HarnessesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            packages: Loadable::Idle,
            target_device: None,
            device_menu_open: false,
            device_menu_pressed_open: false,
            error: None,
            load_task: None,
            action_task: None,
        };
        page.load(cx);
        page
    }

    fn with_target(&self, mut value: serde_json::Value) -> serde_json::Value {
        if let (Some(target), Some(object)) = (&self.target_device, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        self.device_menu_open = false;
        if self.target_device == target {
            cx.notify();
            return;
        }
        self.target_device = target;
        self.error = None;
        self.packages = Loadable::Idle;
        self.load(cx);
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.with_target(serde_json::json!({}));
        self.packages = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_PI_PACKAGES, params)
                .await;
            this.update(cx, |page, cx| {
                page.packages = match result {
                    Ok(value) => match serde_json::from_value::<PiPackagesSnapshot>(value) {
                        Ok(snapshot) => Loadable::Ready(snapshot),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn install_pi(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.with_target(serde_json::json!({}));
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::INSTALL_PI, params).await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => {
                        if let Ok(snapshot) = serde_json::from_value::<PiPackagesSnapshot>(value) {
                            page.packages = Loadable::Ready(snapshot);
                        }
                    }
                    Err(err) => page.error = Some(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn install_package(&mut self, source: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.with_target(serde_json::json!({ "source": source }));
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::INSTALL_PI_PACKAGE, params)
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => {
                        if let Ok(snapshot) = serde_json::from_value::<PiPackagesSnapshot>(value) {
                            page.packages = Loadable::Ready(snapshot);
                        }
                    }
                    Err(err) => page.error = Some(err.to_string()),
                }
                crate::pickers::bump_harness_catalog(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_package_enabled(&mut self, source: String, enabled: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.with_target(serde_json::json!({
            "source": source,
            "enabled": enabled,
        }));
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SET_PI_PACKAGE_ENABLED, params)
                .await;
            this.update(cx, |page, cx| {
                match result {
                    Ok(value) => {
                        if let Ok(snapshot) = serde_json::from_value::<PiPackagesSnapshot>(value) {
                            page.packages = Loadable::Ready(snapshot);
                        }
                    }
                    Err(err) => page.error = Some(err.to_string()),
                }
                crate::pickers::bump_harness_catalog(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_device_switcher(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        use crate::icons::{self, icon};
        let (mut devices, local_id) = {
            let state = self.state.read(cx);
            (state.devices.clone(), state.local_device_id.clone())
        };
        devices.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let effective = self.target_device.clone().or_else(|| local_id.clone());
        let selected = devices
            .iter()
            .find(|d| Some(d.id.as_str()) == effective.as_deref())
            .cloned();
        let glyph = match selected.as_ref().map(|d| d.platform.as_str()) {
            Some("ios") | Some("android") => icons::SMARTPHONE,
            Some("macos") | Some("darwin") => icons::LAPTOP,
            _ => icons::MONITOR,
        };
        let label: SharedString = selected
            .as_ref()
            .map(|d| d.name.clone().into())
            .unwrap_or_else(|| SharedString::from("This device"));
        let open = self.device_menu_open;
        let mut trigger = div()
            .id("harnesses-device-switcher")
            .flex_none()
            .h(px(28.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .bg(if open {
                crate::theme::ink(0.06)
            } else {
                gpui::transparent_black()
            })
            .when(!open, |el| el.hover(|s| s.bg(crate::theme::ink(0.04))))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.device_menu_pressed_open = this.device_menu_open;
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                let pressed_open = std::mem::take(&mut this.device_menu_pressed_open);
                this.device_menu_open = !pressed_open && !this.device_menu_open;
                cx.notify();
            }))
            .child(icon(glyph).size(px(16.0)).text_color(theme.text_muted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(label),
            )
            .child(
                icon(icons::SORT_VERTICAL)
                    .size(px(14.0))
                    .text_color(theme.text_muted.opacity(if open { 0.9 } else { 0.4 })),
            );
        if open {
            let menu = popover::popover_card(theme)
                .w(px(220.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.device_menu_open = false;
                    cx.notify();
                }))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(popover::menu_heading(theme, "Devices"))
                .children(devices.into_iter().enumerate().map(|(ix, device)| {
                    let active = Some(device.id.as_str()) == effective.as_deref();
                    let local = local_id.as_deref() == Some(device.id.as_str());
                    let id = device.id.clone();
                    popover::menu_row(theme, active, format!("agents-device-row-{ix}"))
                        .id(("agents-device-row", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_target_device((!local).then_some(id.clone()), cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .child(SharedString::from(device.name)),
                        )
                        .when(local, |el| {
                            el.child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(theme.text_muted.opacity(0.4))
                                    .child(SharedString::from("You")),
                            )
                        })
                }))
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu("agents-device-menu", menu, None));
        }
        trigger.into_any_element()
    }

    fn action_button(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
        widgets::ghost_action(theme)
            .text_color(theme.text)
            .hover(|s| widgets::ghost_hover(theme, s))
            .child(label.into())
    }

    fn package_tile(theme: &Theme, name: &str, description: Option<&str>) -> gpui::Div {
        match package_icon(name, description) {
            Some(icon) => widgets::row_tile(theme, icon),
            None => widgets::row_tile_letter(theme, package_initial(name)),
        }
    }

    fn package_row(
        &mut self,
        theme: &Theme,
        package: PiPackage,
        index: usize,
        official: bool,
        pi_installed: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let source = package.source.clone();
        let name = package.name.clone();
        let installed = package.installed;
        let enabled = package.enabled;
        let mut title = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(widgets::row_title(theme, name.clone()));
        if let Some(version) = package.version.clone() {
            title = title
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted.opacity(0.45))
                        .child(SharedString::from("·")),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(format!("v{version}"))),
                );
        }
        if official {
            title = title.child(widgets::badge_active(theme, "cypher"));
        }
        let mut row = widgets::card_row(theme, index == 0)
            .id(("pi-package-row", index))
            .child(Self::package_tile(
                theme,
                &name,
                package.description.as_deref(),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(title)
                    .when_some(package.description.clone(), |el, description| {
                        el.child(
                            div()
                                .mt(px(4.0))
                                .w_full()
                                .min_w_0()
                                .overflow_hidden()
                                .truncate()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted.opacity(0.65))
                                .child(SharedString::from(description)),
                        )
                    }),
            );
        if installed {
            row = row.child(
                widgets::toggle_switch(theme, enabled)
                    .flex_none()
                    .id(("pi-package-toggle", index))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_package_enabled(source.clone(), !enabled, cx);
                    })),
            );
        } else {
            let source_for_click = package.source.clone();
            row = row.child(
                Self::action_button(theme, "Install")
                    .flex_none()
                    .id(("pi-package-install", index))
                    .when(!pi_installed, |el| el.opacity(0.45))
                    .when(pi_installed, |el| {
                        el.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.install_package(source_for_click.clone(), cx);
                            }))
                    }),
            );
        }
        row.into_any_element()
    }
}

impl Render for HarnessesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let package_state = self.packages.clone();
        let body: AnyElement = match package_state {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "agents-skeleton",
                    &theme,
                    5,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Loadable::Error(message) => div()
                .child(widgets::error_strip(&theme, message))
                .child(
                    Self::action_button(&theme, "Retry")
                        .id("agents-retry")
                        .mt(px(8.0))
                        .on_click(cx.listener(|page, _, _, cx| page.load(cx))),
                )
                .into_any_element(),
            Loadable::Ready(snapshot) => {
                let extensions: Vec<_> = snapshot
                    .packages
                    .iter()
                    .filter(|p| p.recommended || p.installed)
                    .cloned()
                    .collect();
                let mut content = div().flex().flex_col().gap(px(10.0));
                if !snapshot.pi_installed {
                    let npm_available = snapshot.npm_available;
                    content = content.child(
                        widgets::section_card(&theme).p(px(16.0)).child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(12.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_size(px(13.0))
                                        .text_color(theme.text_muted)
                                        .child(SharedString::from(if npm_available {
                                            "Pi isn't installed yet. Install it to enable extensions."
                                        } else {
                                            "Install Node.js/npm first, then return here to install Pi."
                                        })),
                                )
                                .child(
                                    Self::action_button(&theme, "Install Pi")
                                        .id("install-pi")
                                        .when(!npm_available, |el| el.opacity(0.5))
                                        .when(npm_available, |el| {
                                            el.on_click(cx.listener(|page, _, _, cx| {
                                                page.install_pi(cx)
                                            }))
                                        }),
                                ),
                        ),
                    );
                }
                if !extensions.is_empty() {
                    let extensions_label = widgets::field_label(&theme, "Extensions")
                        .when(snapshot.pi_installed, |el| el.mt(px(24.0)));
                    content = content.child(extensions_label).child(
                        widgets::section_card(&theme).children(
                            extensions.into_iter().enumerate().map(|(ix, package)| {
                                let official = package.recommended;
                                self.package_row(
                                    &theme,
                                    package,
                                    ix,
                                    official,
                                    snapshot.pi_installed,
                                    cx,
                                )
                            }),
                        ),
                    );
                }
                content.into_any_element()
            }
        };
        let switcher = self.render_device_switcher(&theme, cx);
        div()
            .id("harnesses-page")
            .size_full()
            .overflow_y_scroll()
            .child(
            widgets::page_column()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(widgets::page_header(&theme, "Agents", None))
                        .child(switcher),
                )
                .child(
                    widgets::page_subtitle(
                        &theme,
                        "Pi is the primary coding agent. Install Pi and manage its plugins here.",
                    )
                    .max_w(px(560.0))
                    .line_height(px(20.0)),
                )
                .children(
                    self.error
                        .clone()
                        .map(|message| widgets::error_strip(&theme, message).into_any_element()),
                )
                .child(body),
        )
    }
}

fn package_tokens(name: &str, description: Option<&str>) -> Vec<String> {
    let unscoped = name.rsplit('/').next().unwrap_or(name);
    let mut blob = unscoped.to_ascii_lowercase();
    if let Some(description) = description {
        blob.push(' ');
        blob.push_str(&description.to_ascii_lowercase());
    }
    blob.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|tok| !tok.is_empty())
        .map(str::to_string)
        .collect()
}

fn tokens_match(tokens: &[String], keywords: &[&str]) -> bool {
    tokens
        .iter()
        .any(|tok| keywords.iter().any(|key| tok == key))
}

/// Infer a glyph from the package name/description. Unknown third-party
/// packages return `None` so the row can fall back to an initial tile.
pub(crate) fn package_icon(name: &str, description: Option<&str>) -> Option<&'static str> {
    use crate::icons;
    let tokens = package_tokens(name, description);
    const RULES: &[(&[&str], &str)] = &[
        (&["search", "searches", "searching"], icons::MAGNIFER),
        (&["compaction", "compacting"], icons::FOLD_VERTICAL),
        (&["mcp"], icons::COMMAND),
        (
            &[
                "permission",
                "permissions",
                "secret",
                "secrets",
                "credential",
                "credentials",
            ],
            icons::KEY_MINIMALISTIC,
        ),
        (&["squad", "swarm", "multiagent"], icons::CHAT_ROUND_LINE),
        (&["provider", "providers", "newapi"], icons::CLOUD),
        (&["goal", "goals", "todo", "todos"], icons::STAR),
        (&["fast", "turbo", "speed"], icons::TUNING),
        (
            &["ui", "theme", "themes", "sidebar"],
            icons::SIDEBAR_MINIMALISTIC,
        ),
        (
            &["editor", "editors", "vscode", "workspace", "lsp"],
            icons::DOCUMENT,
        ),
        (
            &["review", "reviews", "lint", "linter", "checklist", "codex"],
            icons::CHECKLIST,
        ),
        (&["git", "github", "gitlab"], icons::GIT_BRANCH),
        (&["terminal", "shell", "pty"], icons::TERMINAL),
        (
            &[
                "notify",
                "notification",
                "notifications",
                "slack",
                "discord",
            ],
            icons::BELL,
        ),
        (&["web", "http", "browser", "fetch"], icons::GLOBAL),
        (&["file", "files", "folder", "filesystem"], icons::FOLDER),
    ];
    RULES
        .iter()
        .find(|(keywords, _)| tokens_match(&tokens, keywords))
        .map(|(_, icon)| *icon)
}

pub(crate) fn package_initial(name: &str) -> SharedString {
    let unscoped = name.rsplit('/').next().unwrap_or(name);
    let rest = unscoped
        .strip_prefix("pi-")
        .or_else(|| unscoped.strip_prefix("pi_"))
        .unwrap_or(unscoped);
    rest.chars()
        .find(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons;

    #[test]
    fn recommended_packages_keep_semantic_icons() {
        let cases = [
            (
                "pi-web-search",
                Some("Web search tools for current information."),
                icons::MAGNIFER,
            ),
            (
                "@lll9p/pi-better-compaction",
                Some("More predictable context compaction."),
                icons::FOLD_VERTICAL,
            ),
            (
                "pi-codex-tools",
                Some("Codex-compatible coding and review tools."),
                icons::CHECKLIST,
            ),
            (
                "@narumitw/pi-goal",
                Some("Goal tracking and completion workflow."),
                icons::STAR,
            ),
            (
                "@tunnckocore/pi-gpt-fast-mode",
                Some("Fast model mode controls."),
                icons::TUNING,
            ),
            (
                "pi-mcp-adapter",
                Some("Use MCP servers from Pi."),
                icons::COMMAND,
            ),
            (
                "pi-compact-ui",
                Some("Compact, information-dense Pi UI."),
                icons::SIDEBAR_MINIMALISTIC,
            ),
            (
                "pi-editor-info",
                Some("Editor and workspace context helpers."),
                icons::DOCUMENT,
            ),
            (
                "pi-permission-control",
                Some("Permission prompts and controls."),
                icons::KEY_MINIMALISTIC,
            ),
            (
                "pi-agent-squad",
                Some("Coordinate multiple Pi agents."),
                icons::CHAT_ROUND_LINE,
            ),
            (
                "pi-provider-newapi",
                Some("Additional provider integration."),
                icons::CLOUD,
            ),
        ];
        for (name, description, icon) in cases {
            assert_eq!(package_icon(name, description), Some(icon), "{name}");
        }
    }

    #[test]
    fn third_party_plugins_infer_from_name() {
        assert_eq!(
            package_icon("@acme/pi-brave-search", None),
            Some(icons::MAGNIFER)
        );
        assert_eq!(
            package_icon("pi-github-tools", None),
            Some(icons::GIT_BRANCH)
        );
    }

    #[test]
    fn unknown_third_party_uses_distinctive_initial() {
        assert_eq!(package_icon("@acme/pi-widget-kit", None), None);
        assert_eq!(package_initial("@acme/pi-widget-kit").as_ref(), "W");
        assert_eq!(package_initial("pi-foo").as_ref(), "F");
        assert_eq!(package_initial("strange").as_ref(), "S");
    }

    #[test]
    fn short_keywords_do_not_match_inside_other_words() {
        assert_eq!(package_icon("pi-guide", None), None);
        assert_eq!(package_initial("pi-guide").as_ref(), "G");
    }
}
