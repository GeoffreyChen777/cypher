//! Settings → Devices → Pairing: this device's place in the account's trust
//! group (the engine's `device_trust`). A paired device shows a code to add
//! another one; an unpaired device enters that code to join. The group's
//! "Only paired devices" policy lives here too. Every call goes to this
//! device's own engine: the relay refuses these methods.

use std::time::Duration;

use cypher_engine::device_trust::{PairingStatus, TrustMember, TrustState, TrustStatus};
use cypher_rpc::methods;
use gpui::{AnyElement, Context, Entity, SharedString, Subscription, Task, div, prelude::*, px};

use crate::kit::popover;
use crate::kit::theme::{MonoStyled, Theme};
use crate::settings::widgets;
use crate::state::AppState;
use crate::widgets::text_input::{TextInput, TextInputEvent};

/// The policy switch's caption. Pure so the wording is unit-tested.
pub fn policy_copy() -> &'static str {
    "Your devices accept connections only from each other, over an encrypted channel the server can't read. \
     Devices that aren't paired can't reach them — including the iPhone app until it supports pairing. \
     Applies to every paired device."
}

/// The status line for this device. Pure.
pub fn state_copy(state: TrustState) -> &'static str {
    match state {
        TrustState::Paired => "This device is paired.",
        TrustState::Unpaired => "This device isn't paired with your other devices yet.",
        TrustState::Removed => "This device was removed from your paired devices.",
        TrustState::Unknown => "Checking pairing…",
    }
}

pub struct PairingSection {
    state: Entity<AppState>,
    status: Option<TrustStatus>,
    /// Why the engine can't pair (local workspace, signed out).
    unavailable: Option<SharedString>,
    error: Option<SharedString>,
    busy: bool,
    code_input: Entity<TextInput>,
    confirm_remove: Option<TrustMember>,
    watch: Option<Task<()>>,
    action: Option<Task<()>>,
    _events: Subscription,
}

impl PairingSection {
    /// The latest status from the engine, once one has arrived.
    pub fn status(&self) -> Option<&TrustStatus> {
        self.status.as_ref()
    }

    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let code_input = cx.new(|cx| TextInput::new("XXXX-XXXX-XXXX-XXXX-XXXX", cx));
        let events = cx.subscribe(&code_input, |this: &mut Self, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.join(cx);
            }
        });
        let mut section = Self {
            state,
            status: None,
            unavailable: None,
            error: None,
            busy: false,
            code_input,
            confirm_remove: None,
            watch: None,
            action: None,
            _events: events,
        };
        section.start_watch(cx);
        section
    }

    fn start_watch(&mut self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        self.watch = Some(cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |_, _| {}).is_err() {
                    return;
                }
                let Some(engine) = state.read_with(cx, |state, _| state.engine().cloned()) else {
                    // Still connecting to the engine.
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    continue;
                };
                match engine
                    .client()
                    .subscribe(methods::WATCH_DEVICE_TRUST, serde_json::json!({}))
                    .await
                {
                    Ok(mut rx) => {
                        while let Some(value) = rx.recv().await {
                            let Ok(status) = serde_json::from_value::<TrustStatus>(value) else {
                                continue;
                            };
                            let alive = this.update(cx, |section, cx| {
                                section.unavailable = None;
                                section.status = Some(status);
                                cx.notify();
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                    }
                    Err(err) => {
                        let alive = this.update(cx, |section, cx| {
                            section.unavailable = Some(err.to_string().into());
                            cx.notify();
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(Duration::from_secs(5)).await;
            }
        }));
    }

    fn call(&mut self, method: &'static str, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.action = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(method, params).await;
            this.update(cx, |section, cx| {
                section.busy = false;
                match result.map(serde_json::from_value::<TrustStatus>) {
                    Ok(Ok(status)) => section.status = Some(status),
                    Ok(Err(_)) => {}
                    Err(err) => section.error = Some(err.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn join(&mut self, cx: &mut Context<Self>) {
        let code = self.code_input.read(cx).text().trim().to_string();
        if code.is_empty() {
            return;
        }
        self.code_input
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.call(
            methods::JOIN_DEVICE_GROUP,
            serde_json::json!({ "code": code }),
            cx,
        );
    }

    fn render_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if let Some(message) = &self.unavailable {
            return caption(theme, format!("Pairing isn't available: {message}"))
                .into_any_element();
        }
        let Some(status) = self.status.clone() else {
            return caption(theme, "Checking pairing…").into_any_element();
        };
        let mut body = div().flex().flex_col().gap(px(12.0)).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(widgets::row_title(theme, state_copy(status.state)))
                .child(
                    div()
                        .mono(theme)
                        .text_size(px(11.0))
                        .text_color(theme.text_muted.opacity(0.7))
                        .child(SharedString::from(format!("key {}", status.fingerprint))),
                ),
        );
        if status.conflict {
            body = body.child(widgets::warning_strip(
                theme,
                "The server's list of paired devices disagrees with this device's copy. This device keeps its own copy. If you didn't change your devices, someone may be tampering with the server.",
            ));
        }
        if let Some(error) = &status.error
            && status.state == TrustState::Unknown
        {
            body = body.child(caption(
                theme,
                format!("Couldn't reach the server: {error}"),
            ));
        }
        let body = match status.state {
            TrustState::Paired => self.paired_body(body, &status, theme, cx),
            TrustState::Unpaired | TrustState::Removed => self.join_body(body, &status, theme, cx),
            TrustState::Unknown => body,
        };
        body.into_any_element()
    }

    fn paired_body(
        &mut self,
        mut body: gpui::Div,
        status: &TrustStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        match &status.pairing {
            Some(PairingStatus::Inviting {
                code,
                expires_at,
                request: None,
            }) => {
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(caption(
                            theme,
                            "On the new device, open Settings → Devices and enter this code:",
                        ))
                        .child(
                            div()
                                .mono(theme)
                                .text_size(px(20.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(SharedString::from(code.clone())),
                        )
                        .child(caption(
                            theme,
                            format!("On a Linux device, run cypher pair join {code}"),
                        ))
                        .child(caption(
                            theme,
                            format!(
                                "Waiting for the other device… The code works until {}.",
                                clock(*expires_at)
                            ),
                        ))
                        .child(self.button_row(theme, cx, None, Some("Cancel"))),
                );
            }
            Some(PairingStatus::Inviting {
                request: Some(device),
                ..
            }) => {
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(widgets::row_title(
                            theme,
                            format!("\u{201c}{}\u{201d} wants to join.", device.name),
                        ))
                        .child(caption(
                            theme,
                            format!(
                                "Approve only if this is the device you just entered the code on. Its key is {}; the new device shows the same.",
                                device.fingerprint
                            ),
                        ))
                        .child(self.button_row(theme, cx, Some("Approve"), Some("Decline"))),
                );
            }
            _ => {
                body = body.child(
                    div().flex().flex_row().child(
                        popover::btn_primary(theme, "Add a device")
                            .id("pairing-add")
                            .when(self.busy, |el| el.opacity(0.5))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.call(methods::START_DEVICE_PAIRING, serde_json::json!({}), cx)
                            })),
                    ),
                );
            }
        }

        let mut list = widgets::section_card(theme);
        for (ix, member) in status.members.iter().enumerate() {
            let remove = member.clone();
            list = list.child(
                widgets::card_row(theme, ix == 0)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(theme, member.name.clone()))
                            .child(
                                div()
                                    .mono(theme)
                                    .text_size(px(10.5))
                                    .text_color(theme.text_muted.opacity(0.6))
                                    .child(SharedString::from(format!(
                                        "key {}",
                                        member.fingerprint
                                    ))),
                            ),
                    )
                    .when(member.is_self, |el| {
                        el.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.text_muted)
                                .child("This device"),
                        )
                    })
                    .when(!member.is_self, |el| {
                        el.child(
                            widgets::ghost_action(theme)
                                .id(("pairing-remove", ix))
                                .opacity(0.7)
                                .hover(|s| {
                                    s.opacity(1.0)
                                        .bg(theme.danger.opacity(0.08))
                                        .text_color(theme.danger)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.confirm_remove = Some(remove.clone());
                                    cx.notify();
                                }))
                                .child(SharedString::from("Unpair")),
                        )
                    }),
            );
        }
        body = body.child(list);

        let require = status.require_paired;
        body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            widgets::toggle_switch(theme, require)
                                .id("pairing-require")
                                .cursor_pointer()
                                .when(self.busy, |el| el.opacity(0.5))
                                .when(!self.busy, |el| {
                                    el.on_click(cx.listener(move |this, _, _, cx| {
                                        this.call(
                                            methods::SET_REQUIRE_PAIRED_DEVICES,
                                            serde_json::json!({ "enabled": !require }),
                                            cx,
                                        )
                                    }))
                                }),
                        )
                        .child(widgets::field_label(theme, "Only paired devices")),
                )
                .child(caption(theme, policy_copy())),
        )
    }

    fn join_body(
        &mut self,
        body: gpui::Div,
        status: &TrustStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        match &status.pairing {
            Some(PairingStatus::Joining { error: None }) => body
                .child(caption(
                    theme,
                    format!(
                        "Waiting for approval on your other device. It will show this device's key: {}.",
                        status.fingerprint
                    ),
                ))
                .child(self.button_row(theme, cx, None, Some("Cancel"))),
            other => {
                let failed = match other {
                    Some(PairingStatus::Joining { error: Some(error) }) => Some(error.clone()),
                    _ => None,
                };
                body.child(caption(
                    theme,
                    "On a paired device, open Settings → Devices and click Add a device (on Linux, run cypher pair add). Then enter the code it shows:",
                ))
                .when_some(failed, |el, error| el.child(widgets::warning_strip(theme, error)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().w(px(280.0)).child(popover::dialog_field(
                            self.code_input.clone().into_any_element(),
                        )))
                        .child(
                            popover::btn_primary(theme, "Pair")
                                .id("pairing-join")
                                .when(self.busy, |el| el.opacity(0.5))
                                .on_click(cx.listener(|this, _, _, cx| this.join(cx))),
                        ),
                )
            }
        }
    }

    /// `primary` approves; `secondary` cancels whatever is in progress.
    fn button_row(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
        primary: Option<&'static str>,
        secondary: Option<&'static str>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_row()
            .gap(px(8.0))
            .when_some(primary, |el, label| {
                el.child(
                    popover::btn_primary(theme, label)
                        .id("pairing-approve")
                        .when(self.busy, |el| el.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.call(methods::APPROVE_DEVICE_PAIRING, serde_json::json!({}), cx)
                        })),
                )
            })
            .when_some(secondary, |el, label| {
                el.child(
                    popover::btn_ghost(theme, label, "pairing-cancel")
                        .id("pairing-cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.call(methods::CANCEL_DEVICE_PAIRING, serde_json::json!({}), cx)
                        })),
                )
            })
    }

    fn render_remove_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let member = self.confirm_remove.clone()?;
        let card = popover::dialog_card(theme)
            .child(popover::dialog_title(theme, "Unpair device?"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(
                theme,
                format!(
                    "\u{201c}{}\u{201d} will no longer be trusted by your other devices. To use it with them again, pair it again with a new code.",
                    member.name
                ),
            )))
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "pairing-remove-cancel")
                            .id("pairing-remove-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_remove = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_danger(theme, "Unpair")
                            .id("pairing-remove-confirm")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_remove = None;
                                this.call(
                                    methods::REMOVE_TRUSTED_DEVICE,
                                    serde_json::json!({ "deviceId": member.id }),
                                    cx,
                                );
                            })),
                    ),
            )
            .into_any_element();
        Some(popover::modal("pairing-remove-dialog", viewport, card))
    }
}

fn caption(theme: &Theme, copy: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(12.5))
        .text_color(theme.text_muted)
        .child(copy.into())
}

/// `HH:MM` in local time for an epoch-milliseconds instant.
fn clock(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|at| at.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

impl Render for PairingSection {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let viewport = window.viewport_size();
        let dialog = self.render_remove_dialog(viewport, &theme, cx);
        let body = self.render_body(&theme, cx);
        div()
            .id("device-pairing")
            .mt(px(32.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(widgets::page_header(&theme, "Pairing", None))
            .child(widgets::page_subtitle(
                &theme,
                "Paired devices prove who they are to each other with their own keys, so a stolen sign-in or a compromised server can't control them.",
            ))
            .child(div().mt(px(12.0)).child(body))
            .when_some(self.error.clone(), |el, message| {
                el.child(
                    widgets::error_strip(&theme, message)
                        .id("pairing-error")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        })),
                )
            })
            .when_some(dialog, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_names_each_state_and_the_policy_reach() {
        assert_eq!(state_copy(TrustState::Paired), "This device is paired.");
        assert!(state_copy(TrustState::Removed).contains("removed"));
        assert!(policy_copy().contains("every paired device"));
        assert!(policy_copy().contains("iPhone"));
    }
}
