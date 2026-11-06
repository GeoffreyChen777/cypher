//! Settings → Devices, the pairing half: this device's place in the
//! account's trust group (the engine's `device_trust`). It watches the trust
//! status the device list reads, runs the pairing flows above the list (show
//! a code and approve, or enter one to join), and owns the dialogs for
//! unpairing a device and making one receive-only. Every call goes to this
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
    "Your devices accept connections and chat messages only from each other: connections over an encrypted channel the server can't read, \
     messages signed by the device that sent them. Devices that aren't paired can't reach them — including an iPhone app that isn't paired or up to date. \
     Applies to every paired device."
}

/// Why the policy switch is locked, if it is. Pure.
pub fn policy_lock_copy(status: &TrustStatus) -> Option<&'static str> {
    if status.receive_only {
        Some("Only a device with full control can change this.")
    } else if status.require_paired && status.members.iter().any(|m| m.receive_only) {
        Some(
            "Stays on while a device is receive-only: a device that isn't paired can't be told apart from it.",
        )
    } else {
        None
    }
}

/// The status line for this device when it isn't paired. Pure.
pub fn state_copy(state: TrustState) -> &'static str {
    match state {
        TrustState::Paired => "This device is paired.",
        TrustState::Unpaired => "This device isn't paired with your other devices yet.",
        TrustState::Removed => "This device was removed from your paired devices.",
        TrustState::Unknown => "Checking pairing…",
    }
}

/// The banner on a receive-only device. Pure.
pub fn receive_only_banner() -> &'static str {
    "This device is receive-only. Your other devices can control it, but it can't control them, add or remove devices, or change these settings. A device with full control can change this."
}

/// The receive-only confirmation's body; `turns_on_policy` when the group
/// still lets unpaired devices in. Pure.
pub fn receive_only_copy(name: &str, turns_on_policy: bool) -> String {
    let mut copy = format!(
        "Your other devices can still control \u{201c}{name}\u{201d}: open its sessions and send it messages. It won't be able to control them, add or remove devices, or change these settings."
    );
    if turns_on_policy {
        copy.push_str(
            " This also turns on Only paired devices, so devices that aren't paired — including an iPhone app without pairing — can no longer reach your devices.",
        );
    }
    copy
}

pub struct PairingSection {
    state: Entity<AppState>,
    status: Option<TrustStatus>,
    /// Why the engine can't pair (local workspace, signed out).
    unavailable: Option<SharedString>,
    error: Option<SharedString>,
    busy: bool,
    code_input: Entity<TextInput>,
    confirm_unpair: Option<TrustMember>,
    confirm_receive_only: Option<TrustMember>,
    watch: Option<Task<()>>,
    action: Option<Task<()>>,
    _events: Subscription,
}

impl PairingSection {
    /// The latest status from the engine, once one has arrived.
    pub fn status(&self) -> Option<&TrustStatus> {
        self.status.as_ref()
    }

    /// A pairing call is in flight.
    pub fn busy(&self) -> bool {
        self.busy
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
            confirm_unpair: None,
            confirm_receive_only: None,
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

    // -- actions the device list asks for ------------------------------------

    /// Show a code for a new device to enter.
    pub fn start_invite(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            self.call(methods::START_DEVICE_PAIRING, serde_json::json!({}), cx);
        }
    }

    /// Ask before unpairing `member`.
    pub fn ask_unpair(&mut self, member: TrustMember, cx: &mut Context<Self>) {
        self.confirm_receive_only = None;
        self.confirm_unpair = Some(member);
        cx.notify();
    }

    /// Give `member` full control back, or ask before making it receive-only.
    pub fn set_access(&mut self, member: TrustMember, receive_only: bool, cx: &mut Context<Self>) {
        if self.busy || member.receive_only == receive_only {
            return;
        }
        if receive_only {
            self.confirm_unpair = None;
            self.confirm_receive_only = Some(member);
            cx.notify();
            return;
        }
        self.call(
            methods::SET_DEVICE_RECEIVE_ONLY,
            serde_json::json!({ "deviceId": member.id, "receiveOnly": false }),
            cx,
        );
    }

    /// "Only paired devices", for the whole group.
    pub fn set_require_paired(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !self.busy {
            self.call(
                methods::SET_REQUIRE_PAIRED_DEVICES,
                serde_json::json!({ "enabled": enabled }),
                cx,
            );
        }
    }

    // -- the panel above the device list ---------------------------------------

    fn render_panel(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if let Some(message) = &self.unavailable {
            return Some(caption(
                theme,
                format!("Pairing isn't available: {message}"),
            ));
        }
        let Some(status) = self.status.clone() else {
            return Some(caption(theme, "Checking pairing…"));
        };
        let mut panel = div().flex().flex_col().gap(px(8.0));
        let mut any = false;
        if status.conflict {
            any = true;
            panel = panel.child(widgets::warning_strip(
                theme,
                "The server's list of paired devices disagrees with this device's copy. This device keeps its own copy. If you didn't change your devices, someone may be tampering with the server.",
            ));
        }
        if status.state == TrustState::Paired && status.receive_only {
            any = true;
            panel = panel.child(widgets::warning_strip(theme, receive_only_banner()));
        }
        let flow = match status.state {
            TrustState::Paired => self.invite_flow(&status, theme, cx),
            TrustState::Unpaired | TrustState::Removed => Some(self.join_flow(&status, theme, cx)),
            TrustState::Unknown => Some(caption(
                theme,
                match &status.error {
                    Some(error) => format!("Checking pairing… Couldn't reach the server: {error}"),
                    None => "Checking pairing…".to_string(),
                },
            )),
        };
        if let Some(flow) = flow {
            any = true;
            panel = panel.child(flow);
        }
        any.then_some(panel)
    }

    /// Showing a code, or approving the device that entered it.
    fn invite_flow(
        &mut self,
        status: &TrustStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let body = match &status.pairing {
            Some(PairingStatus::Inviting {
                code,
                expires_at,
                request: None,
            }) => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(widgets::row_title(theme, "Add a device"))
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
                    format!(
                        "On a Linux device, run cypher pair join {code}. Waiting for the other device… The code works until {}.",
                        clock(*expires_at)
                    ),
                ))
                .child(self.button_row(theme, cx, None, Some("Cancel"))),
            Some(PairingStatus::Inviting {
                request: Some(device),
                ..
            }) => div()
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
                        "Approve only if this is the device you just entered the code on. Its key is {}; the new device shows the same. It joins with full control; you can make it receive-only afterwards.",
                        device.fingerprint
                    ),
                ))
                .child(self.button_row(theme, cx, Some("Approve"), Some("Decline"))),
            _ => return None,
        };
        Some(panel_card(theme).child(body))
    }

    fn join_flow(
        &mut self,
        status: &TrustStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let title = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(widgets::row_title(theme, state_copy(status.state)))
            .child(key_label(theme, &status.fingerprint));
        let body = match &status.pairing {
            Some(PairingStatus::Joining { error: None }) => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(title)
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
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(title)
                    .child(caption(
                        theme,
                        "Paired devices prove who they are to each other with their own keys. On a paired device, open Settings → Devices and click Add a device (on Linux, run cypher pair add). Then enter the code it shows:",
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
        };
        panel_card(theme).child(body)
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

    // -- dialogs ----------------------------------------------------------------

    fn render_unpair_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let member = self.confirm_unpair.clone()?;
        let card = popover::dialog_card(theme)
            .child(popover::dialog_title(theme, "Unpair device?"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(
                theme,
                format!(
                    "\u{201c}{}\u{201d} will no longer be trusted by your other devices. It stays signed in to your account. To use it with them again, pair it again with a new code.",
                    member.name
                ),
            )))
            .child(dialog_buttons(
                popover::btn_ghost(theme, "Cancel", "pairing-remove-cancel")
                    .id("pairing-remove-cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_unpair = None;
                        cx.notify();
                    })),
                popover::btn_danger(theme, "Unpair")
                    .id("pairing-remove-confirm")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_unpair = None;
                        this.call(
                            methods::REMOVE_TRUSTED_DEVICE,
                            serde_json::json!({ "deviceId": member.id }),
                            cx,
                        );
                    })),
            ))
            .into_any_element();
        Some(popover::modal("pairing-remove-dialog", viewport, card))
    }

    fn render_receive_only_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let member = self.confirm_receive_only.clone()?;
        let turns_on_policy = self.status.as_ref().is_some_and(|s| !s.require_paired);
        let card = popover::dialog_card(theme)
            .child(popover::dialog_title(theme, "Make device receive-only?"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(
                theme,
                receive_only_copy(&member.name, turns_on_policy),
            )))
            .child(dialog_buttons(
                popover::btn_ghost(theme, "Cancel", "receive-only-cancel")
                    .id("receive-only-cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_receive_only = None;
                        cx.notify();
                    })),
                popover::btn_primary(theme, "Make receive-only")
                    .id("receive-only-confirm")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_receive_only = None;
                        this.call(
                            methods::SET_DEVICE_RECEIVE_ONLY,
                            serde_json::json!({ "deviceId": member.id, "receiveOnly": true }),
                            cx,
                        );
                    })),
            ))
            .into_any_element();
        Some(popover::modal("receive-only-dialog", viewport, card))
    }
}

fn dialog_buttons(cancel: impl IntoElement, confirm: impl IntoElement) -> gpui::Div {
    div()
        .mt(px(16.0))
        .flex()
        .flex_row()
        .justify_end()
        .gap(px(8.0))
        .child(cancel)
        .child(confirm)
}

/// The quiet card a pairing flow sits in, above the device list.
fn panel_card(theme: &Theme) -> gpui::Div {
    div()
        .px(px(20.0))
        .py(px(16.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.card_glass_bg())
}

/// `key 0123456789abcdef` in the quiet mono style rows use.
pub fn key_label(theme: &Theme, fingerprint: &str) -> gpui::Div {
    div()
        .flex_none()
        .mono(theme)
        .text_size(px(10.5))
        .text_color(theme.text_muted.opacity(0.6))
        .child(SharedString::from(format!("key {fingerprint}")))
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
        let unpair = self.render_unpair_dialog(viewport, &theme, cx);
        let receive_only = self.render_receive_only_dialog(viewport, &theme, cx);
        let panel = self.render_panel(&theme, cx);
        let error = self.error.clone();
        div()
            .id("device-pairing")
            .flex()
            .flex_col()
            .when(panel.is_some() || error.is_some(), |el| el.mt(px(16.0)))
            .when_some(panel, |el, panel| el.child(panel))
            .when_some(error, |el, message| {
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
            .when_some(unpair, |el, dialog| el.child(dialog))
            .when_some(receive_only, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(members: &[(&str, bool)], require_paired: bool, receive_only: bool) -> TrustStatus {
        TrustStatus {
            device_id: "me".into(),
            fingerprint: "00".into(),
            state: TrustState::Paired,
            members: members
                .iter()
                .map(|(id, receive_only)| TrustMember {
                    id: (*id).into(),
                    name: (*id).into(),
                    fingerprint: "00".into(),
                    is_self: *id == "me",
                    receive_only: *receive_only,
                })
                .collect(),
            require_paired,
            receive_only,
            pairing: None,
            conflict: false,
            error: None,
        }
    }

    #[test]
    fn copy_names_each_state_and_the_policy_reach() {
        assert_eq!(state_copy(TrustState::Paired), "This device is paired.");
        assert!(state_copy(TrustState::Removed).contains("removed"));
        assert!(policy_copy().contains("every paired device"));
        assert!(policy_copy().contains("iPhone"));
        assert!(policy_copy().contains("signed"));
    }

    #[test]
    fn the_policy_locks_while_a_device_is_receive_only() {
        assert_eq!(
            policy_lock_copy(&status(&[("me", false), ("mac", false)], true, false)),
            None
        );
        assert!(
            policy_lock_copy(&status(&[("me", false), ("mac", true)], true, false))
                .unwrap()
                .contains("receive-only")
        );
        assert!(
            policy_lock_copy(&status(&[("me", true), ("mac", false)], true, true))
                .unwrap()
                .contains("full control")
        );
    }

    #[test]
    fn receive_only_copy_says_what_stays_and_what_goes() {
        let copy = receive_only_copy("vps", false);
        assert!(copy.contains("vps"));
        assert!(copy.contains("can still control"));
        assert!(copy.contains("won't be able to control them"));
        assert!(!copy.contains("Only paired devices"));
        assert!(receive_only_copy("vps", true).contains("Only paired devices"));
    }
}
