//! One device selection shared by Agents, Providers, Commands and MCP.
//! Requests bind an absolute device id before dispatch. Epochs reject late
//! replies; RAII write leases keep the selector locked until a mutation settles.
use crate::{icons, popover, state::AppState, theme::Theme};
use gpui::{Context, Entity, FocusHandle, SharedString, Subscription, Window, div, prelude::*, px};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceTicket {
    pub id: String,
    pub label: String,
    pub local: bool,
    generation: u64,
}

impl DeviceTicket {
    pub fn params(&self, mut value: serde_json::Value) -> serde_json::Value {
        value["targetDeviceId"] = self.id.clone().into();
        value
    }
}

#[derive(Default)]
struct Selection {
    target: Option<String>,
    connection: Option<String>,
    generation: u64,
    writes: Arc<AtomicUsize>,
}

impl Selection {
    fn id(&self) -> Option<&str> {
        self.target.as_deref().or(self.connection.as_deref())
    }

    fn select(&mut self, id: Option<String>) -> Result<bool, String> {
        let id = id.filter(|id| Some(id) != self.connection.as_ref());
        if self.target == id {
            return Ok(false);
        }
        if self.writes.load(Ordering::Acquire) > 0 {
            return Err(
                "Wait for the current device operation to finish before switching machines.".into(),
            );
        }
        self.target = id;
        self.generation = self.generation.wrapping_add(1);
        Ok(true)
    }

    fn matches(&self, ticket: &DeviceTicket) -> bool {
        self.generation == ticket.generation && self.id() == Some(ticket.id.as_str())
    }

    fn lock(&self) -> DeviceWriteLease {
        self.writes.fetch_add(1, Ordering::AcqRel);
        DeviceWriteLease(self.writes.clone())
    }
}

pub struct DeviceWriteLease(Arc<AtomicUsize>);
impl Drop for DeviceWriteLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct DeviceTarget {
    state: Entity<AppState>,
    selection: Selection,
    menu: popover::Popup<()>,
    focus: FocusHandle,
    _state_observer: Subscription,
}

impl DeviceTarget {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let connection = state.read(cx).local_device_id.clone();
        let observer = cx.observe(&state, |this: &mut Self, state, cx| {
            let id = state.read(cx).local_device_id.clone();
            if this.selection.connection != id {
                this.selection.connection = id;
                this.selection.target = None;
                this.selection.generation = this.selection.generation.wrapping_add(1);
            }
            cx.notify();
        });
        Self {
            state,
            selection: Selection {
                connection,
                ..Default::default()
            },
            menu: popover::Popup::default(),
            focus: cx.focus_handle(),
            _state_observer: observer,
        }
    }

    pub fn generation(&self) -> u64 {
        self.selection.generation
    }
    pub fn id(&self) -> Option<&str> {
        self.selection.id()
    }
    pub fn locked(&self) -> bool {
        self.selection.writes.load(Ordering::Acquire) > 0
    }
    pub fn is_local(&self) -> bool {
        self.selection.target.is_none()
    }

    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) -> Result<(), String> {
        if self.selection.select(id)? {
            self.menu = popover::Popup::default();
            cx.notify();
        }
        Ok(())
    }

    pub fn label(&self, cx: &gpui::App) -> String {
        let state = self.state.read(cx);
        self.selection
            .id()
            .map(|id| state.device_name(id).unwrap_or(id).to_string())
            .unwrap_or_else(|| "This device".into())
    }

    pub fn unavailable(&self, cx: &gpui::App) -> Option<String> {
        let state = self.state.read(cx);
        if state.engine().is_none() || self.selection.id().is_none() {
            return Some("Engine not connected. Waiting for device information.".into());
        }
        let id = self.selection.id().unwrap();
        if Some(id) != state.local_device_id.as_deref() {
            if !state.devices.iter().any(|d| d.id == id) {
                return Some(format!(
                    "{} is no longer available. Select another device.",
                    self.label(cx)
                ));
            }
            if !state.device_online(id, chrono::Utc::now()) {
                return Some(format!(
                    "{} is offline. Connect that device to load or change its settings.",
                    self.label(cx)
                ));
            }
        }
        None
    }

    pub fn can_write(&self, cx: &gpui::App) -> bool {
        !self.locked() && self.unavailable(cx).is_none()
    }

    pub fn ticket(&self, cx: &gpui::App) -> Result<DeviceTicket, String> {
        if let Some(error) = self.unavailable(cx) {
            return Err(error);
        }
        Ok(DeviceTicket {
            id: self.selection.id().unwrap().into(),
            label: self.label(cx),
            generation: self.selection.generation,
            local: self.is_local(),
        })
    }

    pub fn matches(&self, ticket: &DeviceTicket) -> bool {
        self.selection.matches(ticket)
    }

    pub fn lock(&mut self, cx: &mut Context<Self>) -> DeviceWriteLease {
        let lease = self.selection.lock();
        self.menu = popover::Popup::default();
        cx.notify();
        lease
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.begin_close() {
            popover::reap_popup(cx, |target: &mut Self| &mut target.menu);
            cx.notify();
        }
    }
}

fn selector_status(locked: bool, available: bool, local: bool) -> &'static str {
    if locked {
        "Updating…"
    } else if !available {
        "Offline"
    } else if local {
        "This device · Online"
    } else {
        "Online"
    }
}

impl Render for DeviceTarget {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let local = state.local_device_id.clone();
        let effective = self.selection.id().map(str::to_string);
        let mut devices = state.devices.clone();
        // The connected engine can be usable before the synced device row lands.
        if let Some(id) = &local
            && !devices.iter().any(|d| &d.id == id)
        {
            devices.push(cypher_proto::Device {
                id: id.clone(),
                name: "This device".into(),
                platform: std::env::consts::OS.into(),
                last_seen_at: None,
                created_at: None,
                version: None,
            });
        }
        devices.sort_by(|a, b| {
            (Some(&b.id) == local.as_ref())
                .cmp(&(Some(&a.id) == local.as_ref()))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.cmp(&b.id))
        });
        let selected = devices.iter().find(|d| Some(&d.id) == effective.as_ref());
        let glyph = match selected.map(|d| d.platform.as_str()) {
            Some("macos" | "darwin") => icons::LAPTOP,
            Some("ios" | "android") => icons::SMARTPHONE,
            _ => icons::MONITOR,
        };
        let locked = self.locked();
        let available = self.unavailable(cx).is_none();
        let status = selector_status(locked, available, self.is_local());
        let mut trigger =
            div()
                .id("settings-device-selector")
                .role(gpui::Role::Button)
                .aria_label(format!("Device settings: {} — {status}", self.label(cx)))
                .track_focus(&self.focus)
                .tab_index(0)
                .tab_stop(!locked)
                .relative()
                .flex_none()
                .w_full()
                .min_w_0()
                .h(px(56.0))
                .px(px(10.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.ink(0.025))
                .flex()
                .items_center()
                .gap(px(10.0))
                .focus_visible(|style| style.border_color(theme.accent))
                .on_key_down(
                    cx.listener(|target, event: &gpui::KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" && target.menu.is_open() {
                            target.close_menu(cx);
                            target.focus.focus(window, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
                .when(locked, |el| el.opacity(0.5))
                .when(!locked, |el| {
                    el.cursor_pointer()
                        .hover(|s| s.bg(theme.element_hover))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|target, _, _, _| target.menu.note_trigger_press()),
                        )
                        .on_click(cx.listener(|target, _, _, cx| {
                            if target.menu.take_press_was_open() {
                                target.close_menu(cx);
                            } else {
                                target.menu.open(());
                                cx.notify();
                            }
                        }))
                })
                .child(
                    div()
                        .size(px(32.0))
                        .flex_none()
                        .rounded(px(8.0))
                        .bg(theme.ink(0.04))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            icons::icon(glyph)
                                .size(px(16.0))
                                .text_color(theme.text_muted),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(self.label(cx))),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().size(px(5.0)).flex_none().rounded_full().bg(
                                    if locked {
                                        theme.accent
                                    } else if available {
                                        theme.success
                                    } else {
                                        theme.text_faint
                                    },
                                ))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(11.0))
                                        .text_color(theme.text_muted)
                                        .child(status),
                                ),
                        ),
                )
                .child(
                    icons::icon(icons::ALT_ARROW_DOWN)
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                );
        if self.menu.get().is_some() {
            let closing = self.menu.closing_since();
            let menu = popover::popover_card(&theme)
                .id("settings-device-options")
                .w(px(264.0))
                .max_h(px(
                    (f32::from(window.viewport_size().height) - 128.0).max(160.0)
                ))
                .overflow_y_scroll()
                .on_mouse_down_out(cx.listener(|target, _, _, cx| target.close_menu(cx)))
                .child(popover::menu_heading(&theme, "Settings for device"))
                .children(devices.into_iter().enumerate().map(|(index, device)| {
                    let local = Some(&device.id) == local.as_ref();
                    let active = Some(&device.id) == effective.as_ref();
                    let online = self
                        .state
                        .read(cx)
                        .device_online(&device.id, chrono::Utc::now());
                    let id = device.id.clone();
                    popover::menu_row(&theme, active, format!("settings-device-{index}"))
                        .id(("settings-device", index))
                        .tab_index(0)
                        .on_click(cx.listener(move |target, _, window, cx| {
                            let _ = target.select(Some(id.clone()), cx);
                            target.close_menu(cx);
                            target.focus.focus(window, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(device.name)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(if local {
                                    "This device"
                                } else if online {
                                    "Online"
                                } else {
                                    "Offline"
                                }),
                        )
                }))
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_below(
                "settings-device-menu",
                menu,
                closing,
            ));
        }
        trigger
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_targets_are_absolute_and_late_responses_are_rejected() {
        let mut selection = Selection {
            connection: Some("a".into()),
            ..Default::default()
        };
        let ticket = DeviceTicket {
            id: "a".into(),
            label: "A".into(),
            local: true,
            generation: 0,
        };
        assert_eq!(ticket.params(serde_json::json!({}))["targetDeviceId"], "a");
        assert!(selection.matches(&ticket));
        selection.select(Some("b".into())).unwrap();
        assert!(!selection.matches(&ticket));
        selection.select(Some("a".into())).unwrap();
        assert!(
            !selection.matches(&ticket),
            "A → B → A must reject the old A reply"
        );
        assert_eq!(selection.target, None);
    }

    #[test]
    fn writes_lock_the_shared_selection_until_all_finish() {
        let mut selection = Selection {
            connection: Some("a".into()),
            ..Default::default()
        };
        let first = selection.lock();
        let second = selection.lock();
        assert!(selection.select(Some("b".into())).is_err());
        drop(first);
        assert!(selection.select(Some("b".into())).is_err());
        drop(second);
        assert!(selection.select(Some("b".into())).unwrap());
    }

    #[test]
    fn missing_remote_target_never_resolves_to_local() {
        let mut selection = Selection {
            connection: Some("a".into()),
            ..Default::default()
        };
        selection.select(Some("deleted-device".into())).unwrap();
        assert_eq!(selection.id(), Some("deleted-device"));
    }

    #[test]
    fn selector_names_its_status_even_without_color() {
        assert_eq!(selector_status(false, true, true), "This device · Online");
        assert_eq!(selector_status(false, true, false), "Online");
        assert_eq!(selector_status(false, false, false), "Offline");
        assert_eq!(selector_status(true, true, false), "Updating…");
    }
}
