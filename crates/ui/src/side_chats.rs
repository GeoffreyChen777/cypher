//! Temporary Side Chats (round 21) — the right-pane panel.
//!
//! A Side Chat is an engine-hosted temporary chat opened from a settled
//! selection (transcript / git diff / terminal). It lives in the right pane
//! as a compact tab, with Promote (expand into a normal root chat on the main
//! surface) and Close (dispose). Until promoted the engine keeps it in host
//! memory only — no sidebar row, no public session.
//!
//! ROUND 21 REFACTOR: the bespoke compact transcript/composer is gone. The
//! panel is now a THIN container that mounts the EXISTING [`crate::transcript::Transcript`]
//! and [`crate::composer::Composer`] against a forked [`AppState`] (see
//! [`AppState::new_side_chat_fork`]) — the same render path and UX as the main
//! chat, with the Composer's RPC transport branched to the engine's private
//! side-chat methods (`SendSideChat` / `InterruptSideChat` /
//! `RespondSideChatInput`). Every RPC still carries `targetDeviceId` when the
//! side chat's host device differs from the connected engine's.
//!
//! The panel renders ONLY: one rounded selection/Open-as-Chat bar, the existing
//! Transcript (flex_1), and the existing Composer. Closing remains in the
//! right-pane tab strip. Send/stop/answer/wizard all flow through the reused
//! components — there is no manual bubble renderer, input, or watcher here.

use gpui::{App, Context, Entity, SharedString, Subscription, Task, div, prelude::*, px};

use cypher_proto::SideChatSource;
use cypher_rpc::methods;

use crate::composer::{Composer, ComposerEvent, ComposerSideChat};
use crate::state::AppState;
use crate::theme::Theme;
use crate::transcript::Transcript;

/// Events the shell listens for.
pub enum SideChatEvent {
    /// Promote succeeded — the chat is now a normal root chat. The shell
    /// selects it on the main surface and closes the tab (dispose-after-
    /// promote is a no-op by engine contract).
    Promoted {
        chat_id: String,
        side_chat_id: String,
    },
    /// The user asked to close the tab — the shell disposes and removes it.
    Close { side_chat_id: String },
}

impl gpui::EventEmitter<SideChatEvent> for SideChatPanel {}

/// A compact, truncated one-line preview of the selected quote the side chat
/// was opened from (pure — testable without a panel). The user sees the
/// imported content at a glance; the ENGINE carries and injects the full text.
pub fn side_chat_quote_preview(selected_text: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 80;
    let single = selected_text.replace('\n', " ");
    if single.chars().count() > MAX_PREVIEW_CHARS {
        let mut out: String = single.chars().take(MAX_PREVIEW_CHARS).collect();
        out.push('…');
        out
    } else {
        single
    }
}

/// The compact header preview for an offering surface (pure — testable
/// without a panel): the source label plus its selection detail.
pub fn side_chat_source_preview(source: &SideChatSource) -> String {
    match source {
        // The anchor is useful to the engine when it gathers bounded parent
        // context, but an internal message id is developer noise in the UI.
        SideChatSource::Transcript { .. } => "Transcript selection".to_string(),
        SideChatSource::GitDiff { scope, file_path } => {
            let mut parts = vec!["Diff selection".to_string()];
            if let Some(scope) = scope {
                parts.push(format!("· {scope}"));
            }
            if let Some(file_path) = file_path {
                parts.push(format!("· {file_path}"));
            }
            parts.join(" ")
        }
        SideChatSource::Terminal { title } => match title {
            Some(title) => format!("Terminal selection · {title}"),
            None => "Terminal selection".to_string(),
        },
    }
}

/// The right-pane Side Chat tab: a thin container over the EXISTING
/// [`Transcript`] (forked state, unique selection scope, no annotations, no
/// rail) and the EXISTING [`Composer`] (side-chat RPC transport), plus the
/// compact rounded selection/Open-as-Chat bar.
pub struct SideChatPanel {
    /// The MAIN app state — engine + local-device identity for lifecycle RPCs
    /// (promote/dispose target the engine directly). Never mutated here.
    state: Entity<AppState>,
    /// The forked/secondary state this panel's Transcript + Composer render
    /// from: shares the engine, owns the synthetic selected row and the
    /// targeted transcript/status watches. The main selection is untouched.
    fork: Entity<AppState>,
    parent_chat_id: String,
    pub side_chat_id: String,
    /// The device hosting the side chat — every side-chat RPC carries this
    /// as `targetDeviceId` when it differs from the connected engine.
    target_device_id: String,
    source: SideChatSource,
    /// The settled selection text, IN FULL, forwarded from the offering
    /// surface through `StartSideChat` — the engine validates it and injects
    /// it into the first send. Rendered here only as a compact preview.
    selected_text: String,
    transcript: Entity<Transcript>,
    composer: Entity<Composer>,
    _composer_events: Subscription,
    send_task: Option<Task<()>>,
    promoting: bool,
    error: Option<SharedString>,
}

impl SideChatPanel {
    pub fn new(
        state: Entity<AppState>,
        parent_chat_id: String,
        side_chat_id: String,
        target_device_id: String,
        source: SideChatSource,
        selected_text: String,
        cx: &mut Context<Self>,
    ) -> Self {
        // Fork the app state (shares the engine, never touches the main
        // selection), then mount the EXISTING Transcript + Composer on it.
        let fork = AppState::new_side_chat_fork(
            &state,
            &parent_chat_id,
            &side_chat_id,
            &target_device_id,
            cx,
        );
        let transcript = cx.new(|cx| Transcript::for_side_chat(fork.clone(), cx));
        let composer = cx.new(|cx| {
            Composer::for_side_chat(
                fork.clone(),
                ComposerSideChat {
                    side_chat_id: side_chat_id.clone(),
                    target_device_id: target_device_id.clone(),
                },
                cx,
            )
        });
        // Every send glides the prompt to the transcript's viewport top and
        // reserves the reply's space below it — same wiring as the main
        // surface, routed to THIS panel's transcript.
        let composer_events = cx.subscribe(&composer, {
            let transcript = transcript.clone();
            move |_this: &mut SideChatPanel, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Sent {
                    chat_id,
                    message_id,
                } => {
                    transcript.update(cx, |t, cx| {
                        t.on_own_send(chat_id.clone(), message_id.clone(), cx)
                    });
                }
            }
        });
        Self {
            state,
            fork,
            parent_chat_id,
            side_chat_id,
            target_device_id,
            source,
            selected_text,
            transcript,
            composer,
            _composer_events: composer_events,
            send_task: None,
            promoting: false,
            error: None,
        }
    }

    /// The tab-strip title: the surface the side chat was opened from.
    pub fn tab_title(&self) -> SharedString {
        SharedString::from(self.source.label())
    }

    /// The chat the side chat was opened from — the parent whose device hosts
    /// it and whose working context the promoted chat inherits.
    pub fn parent_chat_id(&self) -> &str {
        &self.parent_chat_id
    }

    /// Source preview retained for tab/diagnostic consumers.
    pub fn source_preview(&self) -> String {
        side_chat_source_preview(&self.source)
    }

    /// Compact truncated preview of the imported selection (header — the user
    /// sees the content that will be injected on the first send).
    pub fn quote_preview(&self) -> String {
        side_chat_quote_preview(&self.selected_text)
    }

    pub fn source(&self) -> &SideChatSource {
        &self.source
    }

    /// The forked state (promotion handoff: the shell seeds the main state
    /// from it so the promoted chat opens without a blank flash).
    pub fn fork(&self) -> Entity<AppState> {
        self.fork.clone()
    }

    /// The reused Composer (promotion handoff: its draft + staged attachments
    /// ride into the main composer).
    pub fn composer(&self) -> Entity<Composer> {
        self.composer.clone()
    }

    /// The reused Transcript (promotion handoff / testing).
    pub fn transcript(&self) -> Entity<Transcript> {
        self.transcript.clone()
    }

    fn local_device(&self, cx: &App) -> Option<String> {
        self.state.read(cx).local_device_id.clone()
    }

    /// Merge `targetDeviceId` into params when the side chat's host device
    /// differs from the connected engine's own.
    fn with_target(&self, params: &mut serde_json::Map<String, serde_json::Value>, cx: &App) {
        if let Some(local) = self.local_device(cx)
            && self.target_device_id != local
        {
            params.insert(
                "targetDeviceId".into(),
                serde_json::Value::String(self.target_device_id.clone()),
            );
        }
    }

    /// `PromoteSideChat`: expand into a normal root chat. On success the
    /// shell seeds the main state from the fork, selects the chat, and closes
    /// the tab (dispose-after-promote is a no-op by engine contract).
    pub fn promote(&mut self, cx: &mut Context<Self>) {
        if self.promoting {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.promoting = true;
        cx.notify();
        let mut params = serde_json::Map::new();
        params.insert(
            "sideChatId".into(),
            serde_json::Value::String(self.side_chat_id.clone()),
        );
        self.with_target(&mut params, cx);
        let side_chat_id = self.side_chat_id.clone();
        let weak = cx.weak_entity();
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::PROMOTE_SIDE_CHAT,
                    serde_json::Value::Object(params),
                )
                .await;
            match result {
                Ok(value) => {
                    let chat_id = value
                        .get("chatId")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| side_chat_id.clone());
                    this.update(cx, |panel, cx| {
                        panel.promoting = false;
                        cx.emit(SideChatEvent::Promoted {
                            chat_id,
                            side_chat_id: side_chat_id.clone(),
                        });
                        cx.notify();
                    })
                    .ok();
                }
                Err(err) => {
                    if let Some(panel) = weak.upgrade() {
                        panel.update(cx, |panel, cx| {
                            panel.promoting = false;
                            panel.error = Some(format!("Promote failed: {err}").into());
                            cx.notify();
                        });
                    }
                }
            }
        }));
    }

    /// `DisposeSideChat`: tear the temporary chat down (no-op after promote).
    /// Fire-and-forget — the tab is already being removed by the shell.
    pub fn dispose(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "sideChatId".into(),
            serde_json::Value::String(self.side_chat_id.clone()),
        );
        self.with_target(&mut params, cx);
        cx.spawn(async move |_, _| {
            let _ = engine
                .client()
                .call(
                    methods::DISPOSE_SIDE_CHAT,
                    serde_json::Value::Object(params),
                )
                .await;
        })
        .detach();
    }

    /// Rendered panel: a single rounded selection bar with lifecycle actions,
    /// plus the EXISTING Transcript and Composer. No bespoke bubbles, manual
    /// input, or answer prompt — the reused components own all of that.
    pub fn render_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = Theme::of(cx).clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            // Imported-selection preview: rounded wash + the same leading
            // quote rail as the comment editor, plus the promote action.
            // The full selection remains engine-side.
            .child(
                div().px(px(8.0)).py(px(6.0)).child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .pl(px(8.0))
                        .pr(px(5.0))
                        .py(px(5.0))
                        .rounded(px(8.0))
                        .bg(crate::theme::ink(0.035))
                        .child(crate::comments::quote_rail(&theme))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(10.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(self.quote_preview())),
                        )
                        .child(
                            div()
                                .id("side-chat-promote")
                                .px(px(8.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .text_size(px(10.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text_muted)
                                .cursor_pointer()
                                .hover(|style| {
                                    style.bg(crate::theme::ink(0.08)).text_color(theme.text)
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.promote(cx)))
                                .child(SharedString::from("Open as Chat")),
                        ),
                ),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .px(px(10.0))
                        .py(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            // The existing Transcript (flex_1) and the existing Composer —
            // the reused render path and all of its UX.
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
            .child(self.composer.clone())
            .into_any_element()
    }
}

impl gpui::Render for SideChatPanel {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_panel(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_preview_labels_the_offering_surface() {
        // The panel header shows what the side chat was opened from.
        assert_eq!(
            side_chat_source_preview(&SideChatSource::Transcript {
                anchor_message_id: Some("msg-abcdef123456".into()),
            }),
            "Transcript selection"
        );
        assert_eq!(
            side_chat_source_preview(&SideChatSource::Transcript {
                anchor_message_id: None,
            }),
            "Transcript selection"
        );
        assert_eq!(
            side_chat_source_preview(&SideChatSource::GitDiff {
                scope: Some("Working tree".into()),
                file_path: Some("src/lib.rs".into()),
            }),
            "Diff selection · Working tree · src/lib.rs"
        );
        assert_eq!(
            side_chat_source_preview(&SideChatSource::Terminal {
                title: Some("dev server".into()),
            }),
            "Terminal selection · dev server"
        );
    }

    #[test]
    fn quote_preview_truncates_compactly() {
        // The header preview is a compact truncated strip — the engine always
        // carries the FULL text (validated at START), never this copy.
        assert_eq!(side_chat_quote_preview("short quote"), "short quote");
        assert_eq!(
            side_chat_quote_preview("line one\nline two"),
            "line one line two"
        );
        let long = "q".repeat(200);
        let preview = side_chat_quote_preview(&long);
        assert_eq!(preview.chars().count(), 81); // 80 chars + ellipsis
        assert!(preview.ends_with('…'));
    }
}
