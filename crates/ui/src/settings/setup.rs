//! First-run setup for Cypher's isolated Pi Runtime.

use gpui::{
    Context, Entity, EventEmitter, IntoElement, Render, SharedString, Task, Window, div,
    prelude::*, px,
};
use std::time::Duration;

use cypher_engine::pi_packages::PiPackagesSnapshot;
use cypher_engine::pi_runtime::PiRuntimeStatus;
use cypher_rpc::methods;

use crate::icons;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// Shell listens for this and persists `setup_completed`.
#[derive(Debug, Clone)]
pub enum SetupEvent {
    Continue,
    ConfigureProviders,
}

/// Whether the first-run overlay should cover the ready app.
pub fn setup_should_show(completed: bool, debug: bool, dismissed: bool) -> bool {
    !dismissed && (debug || !completed)
}

enum Busy {
    Pi,
}

pub struct SetupPage {
    state: Entity<AppState>,
    packages: Loadable<PiPackagesSnapshot>,
    error: Option<String>,
    busy: Option<Busy>,
    scroll: gpui::ScrollHandle,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
    progress_task: Option<Task<()>>,
    runtime_status: Option<PiRuntimeStatus>,
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
            progress_task: None,
            runtime_status: None,
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
        self.runtime_status = None;
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
        let progress_engine = engine.clone();
        self.progress_task = Some(cx.spawn(async move |this, cx| {
            loop {
                tokio::time::sleep(Duration::from_millis(350)).await;
                let result = progress_engine
                    .client()
                    .call(methods::PI_RUNTIME_STATUS, serde_json::json!({}))
                    .await;
                let keep_polling = this
                    .update(cx, |page, cx| {
                        if !matches!(page.busy, Some(Busy::Pi)) {
                            return false;
                        }
                        if let Ok(value) = result
                            && let Ok(status) = serde_json::from_value::<PiRuntimeStatus>(value)
                        {
                            page.runtime_status = Some(status);
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        }));
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

    fn action_button(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
        widgets::ghost_action(theme)
            .text_color(theme.text)
            .hover(|s| widgets::ghost_hover(theme, s))
            .child(label.into())
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

    fn render_pi_card(
        &mut self,
        theme: &Theme,
        snapshot: &PiPackagesSnapshot,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = matches!(&self.busy, Some(Busy::Pi));
        let copy = if snapshot.pi_installed {
            "Cypher's isolated coding runtime. It does not use your system Pi."
        } else {
            "Download the isolated Node.js, Pi and plugin runtime (about 100 MB)."
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
            let can_install = self.busy.is_none();
            let label = if busy {
                self.runtime_status
                    .as_ref()
                    .and_then(|status| {
                        status.total_bytes.filter(|total| *total > 0).map(|total| {
                            format!(
                                "Downloading… {}%",
                                status.downloaded_bytes.saturating_mul(100) / total
                            )
                        })
                    })
                    .unwrap_or_else(|| "Preparing…".to_string())
            } else {
                "Download runtime".to_string()
            };
            row = row.child(
                popover::btn_primary(theme, &label)
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
                    2,
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
                let mut content = div()
                    .flex()
                    .flex_col()
                    .child(self.render_pi_card(&theme, &snapshot, cx));
                let continue_label = if snapshot.pi_installed {
                    "Add provider"
                } else {
                    "Skip for now"
                };
                let continue_button = if snapshot.pi_installed {
                    popover::btn_primary(&theme, continue_label)
                } else {
                    Self::action_button(&theme, continue_label)
                };
                content = content.child(
                    div().mt(px(28.0)).flex().justify_end().child(
                        continue_button
                            .id("setup-continue")
                            .when(busy, |el| el.opacity(0.5))
                            .when(!busy, |el| {
                                let installed = snapshot.pi_installed;
                                el.on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(if installed {
                                        SetupEvent::ConfigureProviders
                                    } else {
                                        SetupEvent::Continue
                                    });
                                }))
                            }),
                    ),
                );
                if snapshot.pi_installed {
                    content = content.child(
                        div().mt(px(8.0)).flex().justify_end().child(
                            Self::action_button(&theme, "Skip for now")
                                .id("setup-skip")
                                .when(!busy, |el| {
                                    el.on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(SetupEvent::Continue)),
                                    )
                                }),
                        ),
                    );
                }
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
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(560.0))
                            .flex_none()
                            .px(px(8.0))
                            .py(px(32.0))
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
                                        "Cypher uses its own Pi runtime and plugins, isolated from your system installation. Extensions can be managed later in Settings.",
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
