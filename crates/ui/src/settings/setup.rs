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

pub(super) fn runtime_progress_label(status: Option<&PiRuntimeStatus>) -> String {
    let Some(status) = status else {
        return "Preparing…".into();
    };
    match status.total_bytes.filter(|total| *total > 0) {
        Some(total) if status.downloaded_bytes >= total => "Installing…".into(),
        Some(total) => format!(
            "Downloading… {}%",
            status.downloaded_bytes.saturating_mul(100) / total
        ),
        None if status.downloaded_bytes > 0 => format!(
            "Downloading… {:.1} MB",
            status.downloaded_bytes as f64 / 1_000_000.0
        ),
        None => "Preparing…".into(),
    }
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
        self.progress_task = None;
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
                // cx.spawn polls on GPUI's foreground executor, not Tokio.
                // Tokio timers panic here even when the engine has a runtime.
                cx.background_executor()
                    .timer(Duration::from_millis(350))
                    .await;
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
                runtime_progress_label(self.runtime_status.as_ref())
            } else {
                "Download runtime".to_string()
            };
            row = row.child(
                popover::btn_primary(theme, &label)
                    .flex_none()
                    .id("setup-install-pi")
                    .debug_selector(|| "setup-install-pi".into())
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
pub(crate) mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    pub(crate) struct RuntimeFixture {
        pub(crate) installs: AtomicUsize,
        pub(crate) polls: AtomicUsize,
        pub(crate) finish: tokio::sync::Notify,
        pub(crate) requests: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    #[async_trait::async_trait]
    impl cypher_rpc::RpcService for RuntimeFixture {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<cypher_rpc::RpcReply, cypher_rpc::RpcError> {
            self.requests.lock().unwrap().push((method.into(), params));
            let value = match method {
                methods::ENGINE_INFO => serde_json::json!({
                    "deviceId": "setup-test", "workspaceScope": "local"
                }),
                methods::ENGINE_READY => serde_json::json!({}),
                methods::LIST_PI_PACKAGES => serde_json::json!({
                    "piInstalled": false, "npmAvailable": false, "packages": []
                }),
                methods::INSTALL_PI => {
                    let attempt = self.installs.fetch_add(1, Ordering::SeqCst);
                    self.finish.notified().await;
                    if attempt == 0 {
                        return Err(cypher_rpc::RpcError::Failed(
                            "fixture download failed".into(),
                        ));
                    }
                    serde_json::json!({
                        "piInstalled": true, "npmAvailable": true, "packages": []
                    })
                }
                methods::PI_RUNTIME_STATUS => {
                    self.polls.fetch_add(1, Ordering::SeqCst);
                    serde_json::json!({
                        "installed": false, "installing": true,
                        "downloadedBytes": 50, "totalBytes": 100
                    })
                }
                other => return Err(cypher_rpc::RpcError::UnknownMethod(other.into())),
            };
            Ok(cypher_rpc::RpcReply::Value(value))
        }
    }

    pub(crate) fn pump_until(cx: &TestAppContext, predicate: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            if predicate() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "UI fixture timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // Deliberately NOT a tokio::test: install_pi runs on GPUI's foreground
    // executor, where Tokio timers panic even if the engine has a runtime.
    #[gpui::test]
    fn installation_progress_works_without_a_foreground_tokio_runtime(cx: &mut TestAppContext) {
        // The loopback RPC server wakes the UI from a real Tokio worker.
        cx.background_executor.allow_parking();
        let data = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let fixture = Arc::new(RuntimeFixture::default());
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        runtime.spawn(cypher_rpc::serve_ws_listener(listener, fixture.clone()));
        let state = cx.update(|cx| {
            gpui_tokio::init(cx);
            cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
            let state = cx.new(|_| AppState::new());
            AppState::bootstrap(
                state.clone(),
                crate::state::EngineBootConfig {
                    data_dir: data.path().into(),
                    ipc_port: port,
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
        let window = cx.open_window(gpui::size(px(960.0), px(800.0)), |_, cx| {
            SetupPage::new(state, cx)
        });
        let page = window.root(cx).unwrap();
        pump_until(cx, || {
            cx.update(|cx| matches!(page.read(cx).packages, Loadable::Ready(_)))
        });
        assert!(tokio::runtime::Handle::try_current().is_err());
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let button = visual
            .debug_bounds("setup-install-pi")
            .expect("Install button rendered");
        visual.simulate_click(button.center(), Default::default());
        pump_until(cx, || fixture.installs.load(Ordering::SeqCst) == 1);
        cx.background_executor
            .advance_clock(Duration::from_millis(350));
        pump_until(cx, || {
            cx.update(|cx| page.read(cx).runtime_status.is_some())
        });
        assert!(fixture.polls.load(Ordering::SeqCst) > 0);
        fixture.finish.notify_one();
        pump_until(cx, || cx.update(|cx| page.read(cx).busy.is_none()));
        cx.update(|cx| {
            assert_eq!(
                page.read(cx).error.as_deref(),
                Some("fixture download failed")
            );
            assert!(page.read(cx).runtime_status.is_none());
            assert!(page.read(cx).progress_task.is_none());
        });
        // Retry through the same UI control, and reject duplicate starts.
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let button = visual.debug_bounds("setup-install-pi").unwrap();
        visual.simulate_click(button.center(), Default::default());
        page.update(cx, |page, cx| page.install_pi(cx));
        pump_until(cx, || fixture.installs.load(Ordering::SeqCst) == 2);
        cx.background_executor
            .advance_clock(Duration::from_millis(350));
        pump_until(cx, || {
            cx.update(|cx| page.read(cx).runtime_status.is_some())
        });
        fixture.finish.notify_one();
        pump_until(cx, || cx.update(|cx| page.read(cx).busy.is_none()));
        cx.update(|cx| {
            assert!(page.read(cx).error.is_none());
            assert!(page.read(cx).progress_task.is_none());
            assert!(matches!(&page.read(cx).packages, Loadable::Ready(snapshot) if snapshot.pi_installed));
        });
        let polls = fixture.polls.load(Ordering::SeqCst);
        cx.background_executor.advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(fixture.polls.load(Ordering::SeqCst), polls);
    }

    #[test]
    fn runtime_progress_covers_preparing_download_and_activation() {
        assert_eq!(runtime_progress_label(None), "Preparing…");
        let mut status = PiRuntimeStatus {
            installed: false,
            version: None,
            installing: true,
            downloaded_bytes: 50,
            total_bytes: Some(100),
            error: None,
        };
        assert_eq!(runtime_progress_label(Some(&status)), "Downloading… 50%");
        status.downloaded_bytes = 100;
        assert_eq!(runtime_progress_label(Some(&status)), "Installing…");
        status.downloaded_bytes = 110;
        assert_eq!(runtime_progress_label(Some(&status)), "Installing…");
        status.total_bytes = Some(0);
        status.downloaded_bytes = 2_000_000;
        assert_eq!(runtime_progress_label(Some(&status)), "Downloading… 2.0 MB");
    }

    #[test]
    fn first_run_shows_until_dismissed() {
        assert!(setup_should_show(false, false, false));
        assert!(!setup_should_show(true, false, false));
        assert!(!setup_should_show(false, false, true));
        assert!(setup_should_show(true, true, false));
        assert!(!setup_should_show(true, true, true));
    }
}
