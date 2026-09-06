//! Settings → Agents: Pi installation and Pi package management.

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};

use cypher_engine::pi_packages::{PiPackage, PiPackagesSnapshot};
use cypher_engine::pi_runtime::PiRuntimeStatus;
use cypher_rpc::methods;

use super::device_target::DeviceTarget;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct HarnessesPage {
    state: Entity<AppState>,
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
        let mut page = Self {
            state,
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
        self.packages = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_PI_PACKAGES, params)
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
                cx.notify();
            })
            .ok();
        }));
    }

    fn install_pi(&mut self, cx: &mut Context<Self>) {
        self.mutate(methods::INSTALL_PI, serde_json::json!({}), cx);
    }

    fn install_package(&mut self, source: String, cx: &mut Context<Self>) {
        self.mutate(
            methods::INSTALL_PI_PACKAGE,
            serde_json::json!({ "source": source }),
            cx,
        );
    }

    fn set_package_enabled(&mut self, source: String, enabled: bool, cx: &mut Context<Self>) {
        self.mutate(
            methods::SET_PI_PACKAGE_ENABLED,
            serde_json::json!({
                "source": source,
                "enabled": enabled,
            }),
            cx,
        );
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
                page.runtime_status = None;
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
        let blocked = self.busy || !self.target.read(cx).can_write(cx);
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
                    .when(blocked, |el| el.opacity(0.45))
                    .when(!blocked, |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            this.set_package_enabled(source.clone(), !enabled, cx);
                        }))
                    }),
            );
        } else {
            let source_for_click = package.source.clone();
            row = row.child(
                Self::action_button(theme, "Install")
                    .flex_none()
                    .id(("pi-package-install", index))
                    .when(!pi_installed || blocked, |el| el.opacity(0.45))
                    .when(pi_installed && !blocked, |el| {
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
                                            "Download Cypher's isolated Pi runtime to enable the agent and extensions.",
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
                        "Cypher uses an isolated Pi runtime. Download it and manage its plugins here without changing your system Pi.",
                    )
                    .max_w(px(560.0))
                    .line_height(px(20.0)),
                )
                .children(
                    self.error
                        .clone()
                        .map(|message| widgets::error_strip(&theme, message).into_any_element()),
                )
                .when_some(self.target.read(cx).unavailable(cx), |el, error|
                    el.child(widgets::warning_strip(&theme, error)))
                .when(self.busy, |el| el.child(widgets::page_subtitle(&theme, "Updating the selected device…")))
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
                "gpt-fast-pi",
                Some("Provider-agnostic GPT Fast mode controls."),
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
