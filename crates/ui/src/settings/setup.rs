//! First-run setup: install Pi, then pick recommended extensions.

use gpui::{
    Context, Entity, EventEmitter, IntoElement, Render, SharedString, Task, Window, div, prelude::*,
    px,
};

use cypher_engine::pi_packages::{PiPackage, PiPackagesSnapshot};
use cypher_rpc::methods;

use crate::icons;
use crate::popover::{self, Loadable};
use crate::settings::harnesses::{package_icon, package_initial};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// Shell listens for this and persists `setup_completed`.
#[derive(Debug, Clone)]
pub enum SetupEvent {
    Continue,
}

/// Whether the first-run overlay should cover the ready app.
pub fn setup_should_show(completed: bool, debug: bool, dismissed: bool) -> bool {
    !dismissed && (debug || !completed)
}

enum Busy {
    Pi,
    Package(String),
}

pub struct SetupPage {
    state: Entity<AppState>,
    packages: Loadable<PiPackagesSnapshot>,
    error: Option<String>,
    busy: Option<Busy>,
    scroll: gpui::ScrollHandle,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
}

impl EventEmitter<SetupEvent> for SetupPage {}

impl SetupPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            packages: Loadable::Idle,
            error: None,
            busy: None,
            scroll: gpui::ScrollHandle::new(),
            load_task: None,
            action_task: None,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.packages = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_PI_PACKAGES, serde_json::json!({}))
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

    fn apply_snapshot(&mut self, result: Result<serde_json::Value, String>) {
        self.busy = None;
        match result {
            Ok(value) => match serde_json::from_value::<PiPackagesSnapshot>(value) {
                Ok(snapshot) => {
                    self.error = None;
                    self.packages = Loadable::Ready(snapshot);
                }
                Err(err) => self.error = Some(err.to_string()),
            },
            Err(err) => self.error = Some(err),
        }
    }

    fn install_pi(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.error = None;
        self.busy = Some(Busy::Pi);
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::INSTALL_PI, serde_json::json!({}))
                .await
                .map_err(|err| err.to_string());
            this.update(cx, |page, cx| {
                page.apply_snapshot(result);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn install_package(&mut self, source: String, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.error = None;
        self.busy = Some(Busy::Package(source.clone()));
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::INSTALL_PI_PACKAGE,
                    serde_json::json!({ "source": source }),
                )
                .await
                .map_err(|err| err.to_string());
            this.update(cx, |page, cx| {
                page.apply_snapshot(result);
                crate::pickers::bump_harness_catalog(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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

    fn installed_label(theme: &Theme) -> gpui::Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                crate::icons::icon(icons::CHECK)
                    .size(px(14.0))
                    .text_color(theme.success_muted),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from("Installed")),
            )
    }

    fn package_row(
        &mut self,
        theme: &Theme,
        package: PiPackage,
        index: usize,
        pi_installed: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = matches!(&self.busy, Some(Busy::Package(source)) if *source == package.source);
        let blocked = self.busy.is_some() || !pi_installed;
        let mut title = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(widgets::row_title(theme, package.name.clone()));
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
        let mut row = widgets::card_row(theme, index == 0)
            .id(("setup-package-row", index))
            .child(Self::package_tile(
                theme,
                &package.name,
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
        if package.installed {
            row = row.child(Self::installed_label(theme));
        } else {
            let source = package.source.clone();
            let label = if busy { "Installing…" } else { "Install" };
            row = row.child(
                Self::action_button(theme, label)
                    .flex_none()
                    .id(("setup-package-install", index))
                    .when(blocked && !busy, |el| el.opacity(0.45))
                    .when(!blocked || busy, |el| el.cursor_pointer())
                    .when(!blocked, |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            this.install_package(source.clone(), cx);
                        }))
                    }),
            );
        }
        row.into_any_element()
    }

    fn render_pi_card(
        &mut self,
        theme: &Theme,
        snapshot: &PiPackagesSnapshot,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = matches!(&self.busy, Some(Busy::Pi));
        let npm_available = snapshot.npm_available;
        let copy = if snapshot.pi_installed {
            "The primary coding agent. Extensions load through Pi."
        } else if npm_available {
            "The primary coding agent. Install it to enable extensions."
        } else {
            "Install Node.js/npm first, then return here to install Pi."
        };
        let mut row = widgets::card_row(theme, true)
            .id("setup-pi-row")
            .child(widgets::row_tile(theme, icons::PI_MARK))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, "Pi"))
                    .child(
                        div()
                            .mt(px(4.0))
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted.opacity(0.65))
                            .child(SharedString::from(copy)),
                    ),
            );
        if snapshot.pi_installed {
            row = row.child(Self::installed_label(theme));
        } else {
            let can_install = npm_available && self.busy.is_none();
            let label = if busy { "Installing…" } else { "Install Pi" };
            row = row.child(
                popover::btn_primary(theme, label)
                    .flex_none()
                    .id("setup-install-pi")
                    .when(!can_install && !busy, |el| el.opacity(0.5))
                    .when(can_install, |el| {
                        el.on_click(cx.listener(|page, _, _, cx| page.install_pi(cx)))
                    }),
            );
        }
        widgets::section_card(theme)
            .mt(px(28.0))
            .child(row)
            .into_any_element()
    }
}

impl Render for SetupPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let package_state = self.packages.clone();
        let busy = self.busy.is_some();
        let body: gpui::AnyElement = match package_state {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .mt(px(28.0))
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "setup-skeleton",
                    &theme,
                    6,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Loadable::Error(message) => div()
                .mt(px(28.0))
                .child(widgets::error_strip(&theme, message))
                .child(
                    Self::action_button(&theme, "Retry")
                        .id("setup-retry")
                        .mt(px(8.0))
                        .on_click(cx.listener(|page, _, _, cx| page.load(cx))),
                )
                .into_any_element(),
            Loadable::Ready(snapshot) => {
                let extensions: Vec<_> = snapshot
                    .packages
                    .iter()
                    .filter(|package| package.recommended)
                    .cloned()
                    .collect();
                let mut content = div()
                    .flex()
                    .flex_col()
                    .child(self.render_pi_card(&theme, &snapshot, cx));
                if !extensions.is_empty() {
                    content = content
                        .child(widgets::field_label(&theme, "Extensions").mt(px(28.0)))
                        .child(widgets::section_card(&theme).mt(px(10.0)).children(
                            extensions.into_iter().enumerate().map(|(ix, package)| {
                                self.package_row(&theme, package, ix, snapshot.pi_installed, cx)
                            }),
                        ));
                }
                let continue_label = if snapshot.pi_installed {
                    "Continue"
                } else {
                    "Skip for now"
                };
                content = content.child(
                    div()
                        .mt(px(28.0))
                        .flex()
                        .justify_end()
                        .child(
                            popover::btn_primary(&theme, continue_label)
                                .id("setup-continue")
                                .when(busy, |el| el.opacity(0.5))
                                .when(!busy, |el| {
                                    el.on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SetupEvent::Continue);
                                    }))
                                }),
                        ),
                );
                content.into_any_element()
            }
        };

        div()
            .id("setup-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w(px(560.0))
                            .flex_none()
                            .px(px(8.0))
                            .py(px(48.0))
                            .flex()
                            .flex_col()
                            .child(crate::icons::cypher_app_icon().w(px(28.0)).h(px(28.0)))
                            .child(
                                div()
                                    .mt(px(20.0))
                                    .text_size(px(18.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(SharedString::from("Set up Cypher")),
                            )
                            .child(
                                div()
                                    .mt(px(6.0))
                                    .max_w(px(480.0))
                                    .text_size(px(13.0))
                                    .line_height(px(19.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(
                                        "Install Pi, then pick the extensions you want. You can change these later in Settings.",
                                    )),
                            )
                            .children(
                                self.error.clone().map(|message| {
                                    widgets::error_strip(&theme, message).into_any_element()
                                }),
                            )
                            .child(body),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::setup_should_show;

    #[test]
    fn first_run_shows_until_dismissed() {
        assert!(setup_should_show(false, false, false));
        assert!(!setup_should_show(true, false, false));
        assert!(!setup_should_show(false, false, true));
        assert!(setup_should_show(true, true, false));
        assert!(!setup_should_show(true, true, true));
    }
}
