//! Settings → MCP: configured Pi MCP servers, OAuth sign-in, enable/disable.

use gpui::{
    Context, Entity, IntoElement, Render, SharedString, Task, Window, div, prelude::*, px,
};

use cypher_engine::mcp::{McpAuthKind, McpAuthStatus, McpServer, McpSnapshot};
use cypher_rpc::methods;

use crate::icons;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct McpPage {
    state: Entity<AppState>,
    snapshot: Loadable<McpSnapshot>,
    error: Option<String>,
    busy: Option<String>,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
}

impl McpPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            snapshot: Loadable::Idle,
            error: None,
            busy: None,
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
        self.snapshot = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::LIST_MCP_SERVERS, serde_json::json!({}))
                .await;
            this.update(cx, |page, cx| {
                page.snapshot = match result {
                    Ok(value) => match serde_json::from_value::<McpSnapshot>(value) {
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
            Ok(value) => match serde_json::from_value::<McpSnapshot>(value) {
                Ok(snapshot) => {
                    self.error = None;
                    self.snapshot = Loadable::Ready(snapshot);
                }
                Err(err) => self.error = Some(err.to_string()),
            },
            Err(err) => self.error = Some(err),
        }
    }

    fn call(&mut self, method: &'static str, name: String, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.error = None;
        self.busy = Some(name.clone());
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, serde_json::json!({ "name": name }))
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

    fn set_enabled(&mut self, name: String, enabled: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.error = None;
        self.busy = Some(name.clone());
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_MCP_SERVER_ENABLED,
                    serde_json::json!({ "name": name, "enabled": enabled }),
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

    fn status_badge(theme: &Theme, server: &McpServer) -> gpui::Div {
        if !server.enabled {
            return widgets::badge(theme, "disabled");
        }
        match server.auth_status {
            McpAuthStatus::SignedIn => widgets::badge_active(theme, "signed in"),
            McpAuthStatus::NeedsAuth => widgets::badge(theme, "needs auth"),
            McpAuthStatus::Expired => widgets::badge(theme, "expired"),
            McpAuthStatus::NotRequired => widgets::badge(theme, "configured"),
        }
    }

    fn server_row(
        &mut self,
        theme: &Theme,
        server: McpServer,
        index: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let busy = self.busy.as_deref() == Some(server.name.as_str());
        let blocked = self.busy.is_some();
        let icon = if server.transport.starts_with("http") {
            icons::GLOBAL
        } else {
            icons::TERMINAL
        };
        let name = server.name.clone();
        let mut row = widgets::card_row(theme, index == 0)
            .id(("mcp-server-row", index))
            .child(widgets::row_tile(theme, icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(widgets::row_title(theme, name.clone()))
                            .child(Self::status_badge(theme, &server)),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted.opacity(0.65))
                            .child(SharedString::from(server.transport.clone())),
                    ),
            );

        let needs_auth = server.enabled
            && matches!(
                server.auth_kind,
                McpAuthKind::Oauth
            )
            && matches!(
                server.auth_status,
                McpAuthStatus::NeedsAuth | McpAuthStatus::Expired
            );
        let signed_in = server.auth_kind == McpAuthKind::Oauth
            && server.auth_status == McpAuthStatus::SignedIn;

        if needs_auth {
            let label = if busy { "Signing in…" } else { "Sign in" };
            let auth_name = name.clone();
            row = row.child(
                widgets::ghost_action(theme)
                    .flex_none()
                    .id(("mcp-auth", index))
                    .bg(crate::theme::ink(0.08))
                    .text_color(theme.text)
                    .hover(|s| widgets::ghost_hover(theme, s))
                    .when(blocked && !busy, |el| el.opacity(0.5))
                    .when(!blocked, |el| {
                        el.on_click(cx.listener(move |page, _, _, cx| {
                            page.call(methods::START_MCP_AUTH, auth_name.clone(), cx);
                        }))
                    })
                    .child(SharedString::from(label)),
            );
        } else if signed_in {
            let logout_name = name.clone();
            row = row.child(
                widgets::ghost_action(theme)
                    .flex_none()
                    .id(("mcp-logout", index))
                    .text_color(theme.text)
                    .hover(|s| widgets::ghost_hover(theme, s))
                    .when(blocked, |el| el.opacity(0.5))
                    .when(!blocked, |el| {
                        el.on_click(cx.listener(move |page, _, _, cx| {
                            page.call(methods::LOGOUT_MCP_SERVER, logout_name.clone(), cx);
                        }))
                    })
                    .child(SharedString::from("Sign out")),
            );
        }

        let enabled = server.enabled;
        row = row.child(
            widgets::toggle_switch(theme, enabled)
                .flex_none()
                .id(("mcp-toggle", index))
                .when(blocked, |el| el.opacity(0.5))
                .when(!blocked, |el| {
                    el.cursor_pointer()
                        .on_click(cx.listener(move |page, _, _, cx| {
                            page.set_enabled(name.clone(), !enabled, cx);
                        }))
                }),
        );
        row.into_any_element()
    }
}

impl Render for McpPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let body: gpui::AnyElement = match self.snapshot.clone() {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "mcp-skeleton",
                    &theme,
                    3,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Loadable::Error(message) => div()
                .child(widgets::error_strip(&theme, message))
                .child(
                    widgets::ghost_action(&theme)
                        .id("mcp-retry")
                        .mt(px(8.0))
                        .text_color(theme.text)
                        .hover(|s| widgets::ghost_hover(&theme, s))
                        .on_click(cx.listener(|page, _, _, cx| page.load(cx)))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element(),
            Loadable::Ready(snapshot) if snapshot.servers.is_empty() => widgets::section_card(&theme)
                .p(px(16.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(
                            "No MCP servers in ~/.pi/agent/mcp.json yet.",
                        )),
                )
                .into_any_element(),
            Loadable::Ready(snapshot) => {
                let mut content = div().flex().flex_col();
                if !snapshot.adapter_installed {
                    content = content.child(widgets::warning_strip(
                        &theme,
                        "pi-mcp-adapter is not installed. Sign-in needs it — install it in Agents.",
                    ));
                }
                content
                    .child(
                        widgets::section_card(&theme).children(
                            snapshot
                                .servers
                                .into_iter()
                                .enumerate()
                                .map(|(index, server)| self.server_row(&theme, server, index, cx)),
                        ),
                    )
                    .into_any_element()
            }
        };

        div()
            .id("mcp-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(
                        &theme,
                        "MCP",
                        match &self.snapshot {
                            Loadable::Ready(snapshot) => Some(snapshot.servers.len()),
                            _ => None,
                        },
                    ))
                    .child(
                        widgets::page_subtitle(
                            &theme,
                            "Servers from Pi's mcp.json. Sign in with OAuth here; sessions pick them up after a reload.",
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
