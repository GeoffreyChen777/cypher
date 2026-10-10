//! Settings → Agents: Pi installation and Pi package management.

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};

use cypher_engine::pi::packages::PiPackagesSnapshot;
use cypher_engine::pi::runtime::PiRuntimeStatus;
use cypher_rpc::methods;

use super::device_target::DeviceTarget;
use super::titles::TitlesPage;
use super::translation::TranslationControl;
use crate::kit::popover::{self, Loadable};
use crate::kit::theme::Theme;
use crate::settings::widgets;
use crate::state::AppState;

pub struct HarnessesPage {
    state: Entity<AppState>,
    titles: Entity<TitlesPage>,
    translation: Entity<TranslationControl>,
    packages: Loadable<PiPackagesSnapshot>,
    target: Entity<DeviceTarget>,
    generation: u64,
    _target_observer: Subscription,
    busy: bool,
    error: Option<String>,
    load_task: Option<Task<()>>,
    progress_task: Option<Task<()>>,
    runtime_status: Option<PiRuntimeStatus>,
    installing_runtime: bool,
}

impl HarnessesPage {
    pub fn new(
        state: Entity<AppState>,
        target: Entity<DeviceTarget>,
        cx: &mut Context<Self>,
    ) -> Self {
        let generation = target.read(cx).generation();
        let observer = cx.observe(&target, |page: &mut Self, target, cx| {
            let generation = target.read(cx).generation();
            if generation != page.generation {
                page.generation = generation;
                page.load_task = None;
                page.packages = Loadable::Idle;
                page.error = None;
                page.busy = false;
                page.progress_task = None;
                page.runtime_status = None;
                page.installing_runtime = false;
                page.load(cx);
            }
            cx.notify();
        });
        let titles = cx.new(|cx| TitlesPage::new_embedded(state.clone(), target.clone(), cx));
        let translation = cx.new(|cx| TranslationControl::new(state.clone(), target.clone(), cx));
        let mut page = Self {
            state,
            titles,
            translation,
            packages: Loadable::Idle,
            target,
            generation,
            _target_observer: observer,
            busy: false,
            error: None,
            load_task: None,
            progress_task: None,
            runtime_status: None,
            installing_runtime: false,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let ticket = match self.target.read(cx).ticket(cx) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.packages = Loadable::Error(error);
                cx.notify();
                return;
            }
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = ticket.params(serde_json::json!({}));
        let status_params = ticket.params(serde_json::json!({}));
        self.packages = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_PI_PACKAGES, params)
                .await;
            let status = engine
                .client()
                .call(methods::PI_RUNTIME_STATUS, status_params)
                .await;
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.packages = match result {
                    Ok(value) => match serde_json::from_value::<PiPackagesSnapshot>(value) {
                        Ok(snapshot) => Loadable::Ready(snapshot),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(format!("{}: {err}", ticket.label)),
                };
                if !page.installing_runtime {
                    page.runtime_status = status
                        .ok()
                        .and_then(|value| serde_json::from_value::<PiRuntimeStatus>(value).ok());
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn install_pi(&mut self, cx: &mut Context<Self>) {
        self.mutate(methods::INSTALL_PI, serde_json::json!({}), cx);
    }

    fn mutate(&mut self, method: &'static str, params: serde_json::Value, cx: &mut Context<Self>) {
        if self.busy
            || !self.target.read(cx).can_write(cx)
            || self.generation != self.target.read(cx).generation()
        {
            return;
        }
        let Ok(ticket) = self.target.read(cx).ticket(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.load_task = None;
        self.busy = true;
        self.error = None;
        let target = self.target.clone();
        let lease = target.update(cx, |target, cx| target.lock(cx));
        self.installing_runtime = method == methods::INSTALL_PI;
        self.runtime_status = None;
        if self.installing_runtime {
            let progress_engine = engine.clone();
            let progress_ticket = ticket.clone();
            self.progress_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(350))
                        .await;
                    if !this
                        .update(cx, |page, _| page.installing_runtime)
                        .unwrap_or(false)
                    {
                        break;
                    }
                    let result = progress_engine
                        .client()
                        .call(
                            methods::PI_RUNTIME_STATUS,
                            progress_ticket.params(serde_json::json!({})),
                        )
                        .await;
                    let keep_polling = this
                        .update(cx, |page, cx| {
                            if !page.installing_runtime
                                || !page.target.read(cx).matches(&progress_ticket)
                            {
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
        }
        cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, ticket.params(params)).await;
            drop(lease);
            target.update(cx, |_, cx| {
                cx.notify();
                crate::pickers::bump_harness_catalog(cx);
            });
            this.update(cx, |page, cx| {
                if !page.target.read(cx).matches(&ticket) {
                    return;
                }
                page.busy = false;
                page.installing_runtime = false;
                page.progress_task = None;
                match result {
                    Ok(value) => {
                        if let Ok(snapshot) = serde_json::from_value::<PiPackagesSnapshot>(value) {
                            page.packages = Loadable::Ready(snapshot);
                        }
                    }
                    Err(err) => page.error = Some(format!("{}: {err}", ticket.label)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn action_button(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
        widgets::ghost_action(theme)
            .text_color(theme.text)
            .hover(|s| widgets::ghost_hover(theme, s))
            .child(label.into())
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
                    2,
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
                let mut content = div().flex().flex_col().gap(px(10.0));
                if snapshot.pi_installed {
                    let version = self
                        .runtime_status
                        .as_ref()
                        .and_then(|status| status.version.clone())
                        .unwrap_or_else(|| "Installed".into());
                    content = content.child(
                        widgets::section_card(&theme).child(
                            div()
                                .px(px(20.0))
                                .py(px(16.0))
                                .flex()
                                .items_center()
                                .gap(px(12.0))
                                .child(widgets::row_tile(&theme, crate::kit::icons::TUNING))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(widgets::row_title(&theme, "Pi Runtime"))
                                        .child(
                                            div()
                                                .mt(px(3.0))
                                                .text_size(px(12.0))
                                                .text_color(theme.text_muted)
                                                .child(SharedString::from("Isolated coding agent")),
                                        ),
                                )
                                .child(widgets::badge(&theme, version)),
                        ),
                    );
                } else {
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
                                        .child(SharedString::from(
                                            "Download Cypher's isolated Pi runtime to enable the agent.",
                                        )),
                                )
                                .child(
                                    Self::action_button(&theme, if self.installing_runtime {
                                        super::setup::runtime_progress_label(self.runtime_status.as_ref())
                                    } else {
                                        "Download runtime".into()
                                    })
                                        .id("install-pi")
                                        .debug_selector(|| "agents-install-runtime".into())
                                        .when(!self.target.read(cx).can_write(cx) && !self.installing_runtime, |el| el.opacity(0.45))
                                        .on_click(cx.listener(|page, _, _, cx| page.install_pi(cx))),
                                ),
                        ),
                    );
                }
                content.into_any_element()
            }
        };
        div()
            .id("harnesses-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(&theme, "Agents", None))
                    .child(
                        widgets::page_subtitle(
                            &theme,
                            "Manage Cypher's isolated Pi runtime for this device.",
                        )
                        .max_w(px(560.0))
                        .line_height(px(20.0)),
                    )
                    .children(
                        self.error.clone().map(|message| {
                            widgets::error_strip(&theme, message).into_any_element()
                        }),
                    )
                    .when_some(self.target.read(cx).unavailable(cx), |el, error| {
                        el.child(widgets::warning_strip(&theme, error))
                    })
                    .when(self.busy, |el| {
                        el.child(widgets::page_subtitle(
                            &theme,
                            "Updating the selected device…",
                        ))
                    })
                    .child(body)
                    .child(self.translation.clone())
                    .child(div().mt(px(24.0)).child(self.titles.clone())),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn runtime_install_progress_is_device_scoped_and_stops_after_completion(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::settings::setup::tests::{RuntimeFixture, pump_until};
        use gpui::AppContext;
        use std::{
            sync::{Arc, atomic::Ordering},
            time::Duration,
        };

        cx.background_executor.allow_parking();
        let data = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let fixture = Arc::new(RuntimeFixture::default());
        let engine_dir = data.path().to_path_buf();
        std::fs::create_dir_all(&engine_dir).unwrap();
        std::fs::write(engine_dir.join("device-id"), "setup-test").unwrap();
        let port = cypher_env::ipc_socket(&engine_dir).unwrap();
        let listener = runtime
            .block_on(cypher_rpc::LocalListener::bind(&port))
            .unwrap();
        runtime.spawn(listener.serve(fixture.clone()));
        let state = cx.update(|cx| {
            gpui_tokio::init(cx);
            cx.set_global(Theme::for_appearance(crate::kit::theme::Appearance::Dark));
            let state = cx.new(|_| AppState::new());
            AppState::bootstrap(
                state.clone(),
                data.path().join("preferences"),
                crate::state::EngineBootConfig {
                    data_dir: data.path().into(),
                    ipc_socket: port.clone(),
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: cypher_proto::HarnessId::Mock,
                },
                cx,
            );
            state
        });
        pump_until(cx, || cx.update(|cx| state.read(cx).engine().is_some()));
        let target = cx.update(|cx| {
            state.update(cx, |state, _| {
                state.devices.push(
                    serde_json::from_value(serde_json::json!({
                        "id": "remote-test", "name": "Remote Linux", "platform": "linux",
                        "lastSeenAt": chrono::Utc::now(),
                    }))
                    .unwrap(),
                )
            });
            let target = cx.new(|cx| DeviceTarget::new(state.clone(), cx));
            target.update(cx, |target, cx| {
                target.select(Some("remote-test".into()), cx).unwrap()
            });
            target
        });
        let window = cx.open_window(gpui::size(px(960.0), px(800.0)), |_, cx| {
            HarnessesPage::new(state, target.clone(), cx)
        });
        let page = window.root(cx).unwrap();
        pump_until(cx, || {
            cx.update(|cx| matches!(page.read(cx).packages, Loadable::Ready(_)))
        });
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        for attempt in 1..=2 {
            visual.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            let button = visual.debug_bounds("agents-install-runtime").unwrap();
            visual.simulate_click(button.center(), Default::default());
            pump_until(cx, || fixture.installs.load(Ordering::SeqCst) == attempt);
            assert!(
                target
                    .update(cx, |target, cx| target.select(None, cx))
                    .is_err()
            );
            cx.background_executor
                .advance_clock(Duration::from_millis(350));
            pump_until(cx, || {
                cx.update(|cx| page.read(cx).runtime_status.is_some())
            });
            cx.update(|cx| {
                assert_eq!(
                    super::super::setup::runtime_progress_label(
                        page.read(cx).runtime_status.as_ref()
                    ),
                    "Downloading… 50%"
                )
            });
            fixture.finish.notify_one();
            pump_until(cx, || cx.update(|cx| !page.read(cx).busy));
            cx.update(|cx| {
                assert!(page.read(cx).progress_task.is_none());
                assert!(!target.read(cx).locked());
                if attempt == 1 {
                    assert!(
                        page.read(cx)
                            .error
                            .as_ref()
                            .unwrap()
                            .contains("Remote Linux")
                    );
                } else {
                    assert!(
                        matches!(&page.read(cx).packages, Loadable::Ready(s) if s.pi_installed)
                    );
                }
            });
        }
        let requests = fixture.requests.lock().unwrap();
        for (method, params) in requests.iter().filter(|(method, _)| {
            matches!(
                method.as_str(),
                methods::INSTALL_PI | methods::PI_RUNTIME_STATUS
            )
        }) {
            assert_eq!(params["targetDeviceId"], "remote-test", "{method}");
        }
        drop(requests);
        let polls = fixture.polls.load(Ordering::SeqCst);
        cx.background_executor.advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(fixture.polls.load(Ordering::SeqCst), polls);
    }
}
