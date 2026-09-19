//! Settings → Devices (feature-inventory §1.5): the device registry — name,
//! platform, last-seen, presence dot, a "This device" badge, click-to-copy id,
//! a Rename dialog (Mutate renameDevice), and in synced workspaces a Remove
//! action that unpairs another machine (Mutate deleteDevice): that machine
//! signs out of sync and continues in local-only mode. This device cannot be
//! removed from here.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, SharedString, Subscription, Task, Window, div,
    prelude::*, px,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use cypher_proto::WorkspaceScope;
use cypher_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover;
use crate::state::AppState;
use crate::theme::{MonoStyled, Theme};

/// A device that pinged within this window shows a presence dot (engines
/// heartbeat every 15s; 70s tolerates a couple of missed beats).
pub const DEVICE_ONLINE_WINDOW_SECS: i64 = 70;

/// Presence: last-seen within the online window (future timestamps count). Pure.
pub fn device_online(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_seen
        .is_some_and(|at| now.signed_duration_since(at).num_seconds() <= DEVICE_ONLINE_WINDOW_SECS)
}

/// Compact last-seen line. Pure.
pub fn format_last_seen(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(at) = last_seen else {
        return "never seen".to_string();
    };
    let secs = now.signed_duration_since(at).num_seconds();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Scope-aware copy: a local registry describes only the active local
/// workspace and must not imply that account device metadata is already live.
pub fn devices_subtitle(scope: Option<WorkspaceScope>) -> &'static str {
    match scope {
        Some(WorkspaceScope::Local) => "Manage device details stored in this local workspace.",
        Some(WorkspaceScope::Synced | WorkspaceScope::Development) => {
            "Rename devices or remove machines you no longer use."
        }
        None => "Manage device names for this workspace.",
    }
}

/// Remove is a synced-workspace action against *other* machines. Local-only
/// profiles have a single device (this one), and this device re-upserts on boot.
pub fn can_remove_device(scope: Option<WorkspaceScope>, is_local: bool) -> bool {
    !is_local
        && matches!(
            scope,
            Some(WorkspaceScope::Synced | WorkspaceScope::Development)
        )
}

/// Confirm-dialog body. Pure so the wording is unit-tested.
pub fn delete_device_copy(name: &str) -> String {
    format!(
        "Removing \u{201c}{name}\u{201d} signs that machine out of this account. It will continue in local-only mode. Signing in again there will re-pair it."
    )
}

/// Where one device stands in the fleet-update flow (Devices → Update).
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceUpdate {
    Checking,
    UpToDate,
    /// A newer release is published for that device.
    Available(String),
    /// ApplyUpdate is in flight (download + swap on the device).
    Applying,
    /// The device accepted the update and is restarting into `target`.
    Restarting {
        target: String,
        since: Instant,
    },
    /// `busy`: the device refused because runs/terminals are active — the
    /// row offers "Update anyway".
    Failed {
        message: String,
        busy: bool,
    },
}

/// A restarting device is done once its registry row reports the target
/// version; after [`RESTART_TIMEOUT`] without it, the row says so. Pure.
pub const RESTART_TIMEOUT: Duration = Duration::from_secs(240);

pub fn restart_resolved(
    phase: &DeviceUpdate,
    device_version: Option<&str>,
    now: Instant,
) -> Option<DeviceUpdate> {
    let DeviceUpdate::Restarting { target, since } = phase else {
        return None;
    };
    if device_version == Some(target.as_str()) {
        return Some(DeviceUpdate::UpToDate);
    }
    (now.duration_since(*since) >= RESTART_TIMEOUT).then(|| DeviceUpdate::Failed {
        message: format!("Not back on {target} yet — check the device"),
        busy: false,
    })
}

/// Badge copy for a device's update phase. Pure.
pub fn update_badge(phase: &DeviceUpdate) -> String {
    match phase {
        DeviceUpdate::Checking => "Checking…".into(),
        DeviceUpdate::UpToDate => "Up to date".into(),
        DeviceUpdate::Available(latest) => format!("{latest} available"),
        DeviceUpdate::Applying => "Updating…".into(),
        DeviceUpdate::Restarting { target, .. } => format!("Restarting into {target}…"),
        DeviceUpdate::Failed { message, .. } => message.clone(),
    }
}

/// ApplyUpdate error text that means "idle guard", not a real failure.
pub fn is_busy_error(message: &str) -> bool {
    message.contains("busy")
}

struct RenameDialog {
    device_id: String,
    input: Entity<ComposerInput>,
    _events: Subscription,
}

pub struct DevicesPage {
    state: Entity<AppState>,
    rename: Option<RenameDialog>,
    /// Device id waiting on the remove confirmation dialog.
    delete_confirm: Option<String>,
    /// Device id whose id-chip shows "Copied" right now.
    copied: Option<String>,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    copy_task: Option<Task<()>>,
    /// Per-device fleet-update phase (Devices → Update).
    updates: HashMap<String, DeviceUpdate>,
    update_tasks: Vec<Task<()>>,
    /// The first render with a device list runs one check across the fleet.
    auto_checked: bool,
    _observe: Subscription,
}

impl DevicesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, state, cx| {
            this.reconcile_restarts(&state, cx);
            cx.notify()
        });
        Self {
            state,
            rename: None,
            delete_confirm: None,
            copied: None,
            error: None,
            task: None,
            copy_task: None,
            updates: HashMap::new(),
            update_tasks: Vec::new(),
            auto_checked: false,
            _observe: observe,
        }
    }

    fn open_rename(&mut self, device_id: String, current: String, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        let input = cx.new(|cx| ComposerInput::new("Device name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename(cx);
            }
        });
        self.rename = Some(RenameDialog {
            device_id,
            input,
            _events: events,
        });
        cx.notify();
    }

    fn submit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename.take() else {
            return;
        };
        let name = dialog.input.read(cx).text().trim().to_string();
        if name.is_empty() {
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({
            "op": "renameDevice",
            "deviceId": dialog.device_id,
            "name": name,
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |page, cx| {
                if let Err(err) = result {
                    page.error = Some(format!("Rename failed: {err}").into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn open_delete(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.rename = None;
        self.delete_confirm = Some(device_id);
        cx.notify();
    }

    fn submit_delete(&mut self, cx: &mut Context<Self>) {
        let Some(device_id) = self.delete_confirm.take() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::json!({
            "op": "deleteDevice",
            "deviceId": device_id,
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |page, cx| {
                if let Err(err) = result {
                    page.error = Some(format!("Remove failed: {err}").into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- fleet updates ----

    /// Restarting rows resolve from the registry: the device's engine writes
    /// its version on boot.
    fn reconcile_restarts(&mut self, state: &Entity<AppState>, cx: &App) {
        let now = Instant::now();
        let versions: HashMap<String, Option<String>> = state
            .read(cx)
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.version.clone()))
            .collect();
        for (id, phase) in self.updates.iter_mut() {
            if let Some(next) =
                restart_resolved(phase, versions.get(id).and_then(|v| v.as_deref()), now)
            {
                *phase = next;
            }
        }
    }

    fn target_params(
        &self,
        device_id: &str,
        cx: &App,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut params = serde_json::Map::new();
        if self.state.read(cx).local_device_id.as_deref() != Some(device_id) {
            params.insert("targetDeviceId".into(), device_id.into());
        }
        params
    }

    /// Ask every online device whether a newer release is published for it.
    pub fn check_all(&mut self, cx: &mut Context<Self>) {
        let now = Utc::now();
        let ids: Vec<String> = self
            .state
            .read(cx)
            .devices
            .iter()
            .filter(|d| d.platform != "ios" && d.platform != "android")
            .filter(|d| device_online(d.last_seen_at, now))
            .map(|d| d.id.clone())
            .collect();
        for id in ids {
            self.check_device(id, cx);
        }
    }

    fn check_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if matches!(
            self.updates.get(&device_id),
            Some(DeviceUpdate::Applying | DeviceUpdate::Restarting { .. })
        ) {
            return;
        }
        self.updates
            .insert(device_id.clone(), DeviceUpdate::Checking);
        let params = serde_json::Value::Object(self.target_params(&device_id, cx));
        self.update_tasks.push(cx.spawn(async move |this, cx| {
            let call = engine.client().call(methods::CHECK_UPDATE, params);
            let deadline = cx.background_executor().timer(Duration::from_secs(30));
            futures::pin_mut!(call);
            futures::pin_mut!(deadline);
            let phase = match futures::future::select(call, deadline).await {
                futures::future::Either::Left((Ok(value), _)) => {
                    match serde_json::from_value::<cypher_update::UpdateStatus>(value) {
                        Ok(status) => match (status.update_available, status.latest_version) {
                            (true, Some(latest)) => DeviceUpdate::Available(latest),
                            _ if status.error.is_some() => DeviceUpdate::Failed {
                                message: status.error.unwrap_or_default(),
                                busy: false,
                            },
                            _ => DeviceUpdate::UpToDate,
                        },
                        Err(err) => DeviceUpdate::Failed {
                            message: format!("Unreadable reply: {err}"),
                            busy: false,
                        },
                    }
                }
                futures::future::Either::Left((Err(err), _)) => DeviceUpdate::Failed {
                    message: format!("Check failed: {err}"),
                    busy: false,
                },
                futures::future::Either::Right(_) => DeviceUpdate::Failed {
                    message: "Check timed out".into(),
                    busy: false,
                },
            };
            this.update(cx, |page, cx| {
                if page.updates.get(&device_id) == Some(&DeviceUpdate::Checking) {
                    page.updates.insert(device_id.clone(), phase);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Apply the published release on one device: Linux services swap and
    /// restart themselves; a Mac swaps its bundle and relaunches. Without
    /// `force` the device refuses while runs or terminals are active.
    fn apply_device(&mut self, device_id: String, force: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let target = match self.updates.get(&device_id) {
            Some(DeviceUpdate::Available(latest)) => latest.clone(),
            Some(DeviceUpdate::Failed { .. }) => String::new(),
            _ => return,
        };
        self.updates
            .insert(device_id.clone(), DeviceUpdate::Applying);
        let mut params = self.target_params(&device_id, cx);
        params.insert("force".into(), force.into());
        self.update_tasks.push(cx.spawn(async move |this, cx| {
            let call = engine
                .client()
                .call(methods::APPLY_UPDATE, serde_json::Value::Object(params));
            let deadline = cx.background_executor().timer(Duration::from_secs(20 * 60));
            futures::pin_mut!(call);
            futures::pin_mut!(deadline);
            let phase = match futures::future::select(call, deadline).await {
                futures::future::Either::Left((Ok(value), _)) => DeviceUpdate::Restarting {
                    target: value["version"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or(target),
                    since: Instant::now(),
                },
                futures::future::Either::Left((Err(err), _)) => {
                    let message = err.to_string();
                    DeviceUpdate::Failed {
                        busy: is_busy_error(&message),
                        message,
                    }
                }
                futures::future::Either::Right(_) => DeviceUpdate::Failed {
                    message: "No reply from the device — it may still be updating".into(),
                    busy: false,
                },
            };
            this.update(cx, |page, cx| {
                page.updates.insert(device_id.clone(), phase);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn apply_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .updates
            .iter()
            .filter(|(_, phase)| matches!(phase, DeviceUpdate::Available(_)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.apply_device(id, false, cx);
        }
    }

    fn copy_id(&mut self, device_id: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(device_id.clone()));
        self.copied = Some(device_id);
        self.copy_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |page, cx| {
                page.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_rename_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let dialog = self.rename.as_ref()?;
        let input = dialog.input.clone();
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, "Rename device"))
            .child(
                div()
                    .mt(px(12.0))
                    .child(popover::dialog_field(input.into_any_element())),
            )
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "rename-cancel")
                            .id("rename-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.rename = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, "Rename")
                            .id("rename-save")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_rename(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("rename-device-dialog", viewport, card))
    }

    fn render_delete_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        let device_id = self.delete_confirm.clone()?;
        let name = {
            let state = self.state.read(cx);
            state
                .devices
                .iter()
                .find(|device| device.id == device_id)
                .map(|device| device.name.clone())
                .unwrap_or_else(|| "this machine".into())
        };
        let copy = delete_device_copy(&name);
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, "Remove device?"))
            .child(div().mt(px(6.0)).child(popover::dialog_body(&theme, copy)))
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "delete-device-cancel")
                            .id("delete-device-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.delete_confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_danger(&theme, "Remove")
                            .id("delete-device-confirm")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_delete(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("delete-device-dialog", viewport, card))
    }
}

/// Human platform label (zeron settings.devices.tsx `platformLabel`).
pub fn platform_label(platform: &str) -> &str {
    match platform {
        "macos" | "darwin" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        "web" => "Web",
        "ios" => "iOS",
        "android" => "Android",
        other => other,
    }
}

/// Short device id for the click-to-copy chip (`abcd1234…wxyz`).
pub fn short_id(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…{}", &id[..8], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

impl Render for DevicesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::settings::widgets;
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let (devices, local_id, workspace_scope) = {
            let state = self.state.read(cx);
            (
                state.devices.clone(),
                state.local_device_id.clone(),
                state.workspace_scope,
            )
        };
        let copied = self.copied.clone();
        let viewport = window.viewport_size();
        let dialog = self.render_rename_dialog(viewport, cx);
        let delete_dialog = self.render_delete_dialog(viewport, cx);
        let emerald = theme.success; // emerald-400
        let count = devices.len();
        if !self.auto_checked && count > 0 && self.state.read(cx).engine().is_some() {
            self.auto_checked = true;
            self.check_all(cx);
        }
        let updates = self.updates.clone();
        let available_count = updates
            .values()
            .filter(|phase| matches!(phase, DeviceUpdate::Available(_)))
            .count();
        let checking = updates
            .values()
            .any(|phase| matches!(phase, DeviceUpdate::Checking));

        let rows: Vec<AnyElement> = devices
            .into_iter()
            .enumerate()
            .map(|(ix, device)| {
                let online = device_online(device.last_seen_at, now);
                let is_local = local_id.as_deref() == Some(device.id.as_str());
                let id_copied = copied.as_deref() == Some(device.id.as_str());
                let copy_id = device.id.clone();
                let rename_id = device.id.clone();
                let rename_name = device.name.clone();
                let remove_id = device.id.clone();
                let update_id = device.id.clone();
                let show_remove = can_remove_device(workspace_scope, is_local);
                let phase = updates.get(&device.id).cloned();
                let update_action: Option<(&'static str, bool)> = match &phase {
                    Some(DeviceUpdate::Available(_)) => Some(("Update", false)),
                    Some(DeviceUpdate::Failed { busy: true, .. }) => Some(("Update anyway", true)),
                    Some(DeviceUpdate::Failed { busy: false, .. }) => Some(("Retry", false)),
                    _ => None,
                };
                let platform_icon = match device.platform.as_str() {
                    "macos" | "darwin" => crate::icons::LAPTOP,
                    "web" => crate::icons::GLOBAL,
                    "ios" | "android" => crate::icons::SMARTPHONE,
                    _ => crate::icons::MONITOR,
                };
                // Presence lives ON the identity tile: a corner dot (emerald
                // online with a soft glow, faint offline), ringed by the card
                // tone so it "cuts" the tile — zeron settings.devices.tsx
                // `border-2 border-[var(--card)]` +
                // `shadow-[0_0_6px_rgba(52,211,153,0.55)]`.
                let tile = widgets::row_tile(&theme, platform_icon).relative().child(
                    div()
                        .absolute()
                        .bottom(px(-3.0))
                        .right(px(-3.0))
                        .size(px(9.0))
                        .rounded_full()
                        .border_2()
                        .border_color(theme.surface)
                        .when(online, |el| {
                            el.bg(emerald).shadow(vec![gpui::BoxShadow {
                                color: emerald.opacity(0.55),
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(6.0),
                                spread_radius: px(0.0),
                                inset: false,
                            }])
                        })
                        .when(!online, |el| el.bg(crate::theme::ink(0.22))),
                );
                // One quiet meta line: platform · version · (offline: last
                // seen) · id chip.
                let mut meta: Vec<AnyElement> = vec![
                    div()
                        .child(SharedString::from(
                            platform_label(&device.platform).to_string(),
                        ))
                        .into_any_element(),
                ];
                if let Some(version) = device.version.as_deref().filter(|v| !v.is_empty()) {
                    meta.push(
                        div()
                            .child(SharedString::from(format!("v{version}")))
                            .into_any_element(),
                    );
                }
                // Update state rides a right-aligned pill (below), never the
                // meta line: a long status pushed the id chip onto its own
                // row and rows stopped lining up.
                let status_pill = phase.as_ref().map(|phase| {
                    let tone = match phase {
                        DeviceUpdate::Available(_) => theme.accent,
                        DeviceUpdate::Failed { .. } => theme.danger,
                        DeviceUpdate::UpToDate => theme.success_muted,
                        _ => theme.text_muted,
                    };
                    div()
                        .flex_none()
                        .max_w(px(220.0))
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded_full()
                        .bg(tone.opacity(0.10))
                        .text_size(px(10.5))
                        .text_color(tone)
                        .truncate()
                        .child(SharedString::from(update_badge(phase)))
                });
                if !online {
                    meta.push(
                        div()
                            .child(SharedString::from(format!(
                                "Last seen {}",
                                format_last_seen(device.last_seen_at, now)
                            )))
                            .into_any_element(),
                    );
                }
                // "Added {time ago}" — always present (zeron settings.devices.tsx).
                if let Some(created) = device.created_at {
                    meta.push(
                        div()
                            .child(SharedString::from(format!(
                                "Added {}",
                                format_last_seen(Some(created), now)
                            )))
                            .into_any_element(),
                    );
                }
                // The id chip ends the meta line; with the update state on
                // its own pill the line is short enough to stay one row.
                let id_chip = div()
                    .id(("device-id", ix))
                    .flex_none()
                    .mono(&theme)
                    .text_size(px(10.5))
                    .text_color(if id_copied {
                        theme.success_muted.opacity(0.9)
                    } else {
                        theme.text_muted.opacity(0.5)
                    })
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text_muted))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_id(copy_id.clone(), cx);
                    }))
                    .child(SharedString::from(if id_copied {
                        "Copied".to_string()
                    } else {
                        short_id(&device.id)
                    }));
                meta.push(id_chip.into_any_element());

                widgets::card_row(&theme, ix == 0)
                    .child(tile)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, device.name.clone()))
                            .child(widgets::meta_line(&theme, meta)),
                    )
                    .when_some(status_pill, |el, pill| el.child(pill))
                    .when(is_local, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(10.5))
                                .text_color(theme.text_muted)
                                .child(if workspace_scope == Some(WorkspaceScope::Local) {
                                    "Local only"
                                } else {
                                    "This device"
                                }),
                        )
                    })
                    .when_some(update_action, |el, (label, force)| {
                        let retry_check = label == "Retry";
                        let id = update_id.clone();
                        el.child(
                            widgets::ghost_action(&theme)
                                .id(("device-update", ix))
                                .text_color(theme.accent)
                                .hover(|s| {
                                    s.bg(theme.accent.opacity(0.10)).text_color(theme.accent)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if retry_check {
                                        this.check_device(id.clone(), cx);
                                    } else {
                                        this.apply_device(id.clone(), force, cx);
                                    }
                                }))
                                .child(
                                    crate::icons::icon(crate::icons::ARCHIVE_UP_MINIMALISTIC)
                                        .size(px(14.0))
                                        .text_color(theme.accent),
                                )
                                .child(SharedString::from(label)),
                        )
                    })
                    .child(
                        // `opacity-70 hover:opacity-100` (zeron: also rises on
                        // row hover — gpui has no group-hover, so the button's
                        // own hover carries the reveal).
                        widgets::ghost_action(&theme)
                            .id(("device-rename", ix))
                            .opacity(0.7)
                            .hover(|s| {
                                s.opacity(1.0)
                                    .bg(crate::theme::ink(0.06))
                                    .text_color(theme.text)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_rename(rename_id.clone(), rename_name.clone(), cx);
                            }))
                            .child(
                                crate::icons::icon(crate::icons::PEN)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Rename")),
                    )
                    .when(show_remove, |el| {
                        el.child(
                            widgets::ghost_action(&theme)
                                .id(("device-remove", ix))
                                .opacity(0.7)
                                .hover(|s| {
                                    s.opacity(1.0)
                                        .bg(theme.danger.opacity(0.08))
                                        .text_color(theme.danger)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_delete(remove_id.clone(), cx);
                                }))
                                .child(
                                    crate::icons::icon(crate::icons::TRASH_BIN_MINIMALISTIC)
                                        .size(px(14.0))
                                        .text_color(theme.danger),
                                )
                                .child(SharedString::from("Remove")),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        let card = widgets::section_card(&theme);
        let card = if rows.is_empty() {
            card.child(
                div()
                    .px(px(20.0))
                    .py(px(40.0))
                    .text_center()
                    .text_size(px(14.0))
                    .text_color(theme.text_muted.opacity(0.6))
                    .child(SharedString::from("No devices registered")),
            )
        } else {
            card.children(rows)
        };

        div()
            .id("devices-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                widgets::page_column()
                    .child(widgets::page_header(
                        &theme,
                        "Devices",
                        (count > 0).then_some(count),
                    ))
                    .child(widgets::page_subtitle(
                        &theme,
                        devices_subtitle(workspace_scope),
                    ))
                    // Fleet updates: check every online device, update the
                    // ones with a newer release. Each device applies its own
                    // release and restarts itself; iOS updates through
                    // TestFlight and is left out.
                    .when(count > 0, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .pb(px(12.0))
                                .child(
                                    widgets::ghost_action(&theme)
                                        .id("devices-check-updates")
                                        .when(checking, |el| el.opacity(0.6))
                                        .on_click(cx.listener(|this, _, _, cx| this.check_all(cx)))
                                        .child(
                                            crate::icons::icon(crate::icons::REFRESH)
                                                .size(px(14.0))
                                                .text_color(theme.text_muted),
                                        )
                                        .child(SharedString::from(if checking {
                                            "Checking…"
                                        } else {
                                            "Check for updates"
                                        })),
                                )
                                .when(available_count > 0, |el| {
                                    el.child(
                                        widgets::ghost_action(&theme)
                                            .id("devices-update-all")
                                            .text_color(theme.accent)
                                            .hover(|s| {
                                                s.bg(theme.accent.opacity(0.10))
                                                    .text_color(theme.accent)
                                            })
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.apply_all(cx)),
                                            )
                                            .child(
                                                crate::icons::icon(
                                                    crate::icons::ARCHIVE_UP_MINIMALISTIC,
                                                )
                                                .size(px(14.0))
                                                .text_color(theme.accent),
                                            )
                                            .child(SharedString::from(format!(
                                                "Update all ({available_count})"
                                            ))),
                                    )
                                }),
                        )
                    })
                    .when_some(self.error.clone(), |el, message| {
                        el.child(
                            widgets::error_strip(&theme, message)
                                .id("devices-error")
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.error = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(card),
            )
            .when_some(dialog, |el, dialog| el.child(dialog))
            .when_some(delete_dialog, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn presence_window() {
        let now = Utc::now();
        assert!(device_online(Some(now - TimeDelta::seconds(10)), now));
        assert!(device_online(Some(now - TimeDelta::seconds(70)), now));
        assert!(!device_online(Some(now - TimeDelta::seconds(71)), now));
        assert!(!device_online(None, now));
        // Clock skew (future) counts as online.
        assert!(device_online(Some(now + TimeDelta::seconds(30)), now));
    }

    #[test]
    fn last_seen_formatting() {
        let now = Utc::now();
        assert_eq!(format_last_seen(None, now), "never seen");
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::seconds(30)), now),
            "just now"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::minutes(5)), now),
            "5m ago"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::hours(3)), now),
            "3h ago"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::days(2)), now),
            "2d ago"
        );
    }

    #[test]
    fn local_subtitle_does_not_claim_synced_metadata() {
        let copy = devices_subtitle(Some(WorkspaceScope::Local));
        assert!(copy.contains("local workspace"));
        assert!(!copy.contains("synced"));
        assert!(!copy.contains("remove"));
    }

    #[test]
    fn synced_subtitle_mentions_removal() {
        let copy = devices_subtitle(Some(WorkspaceScope::Synced));
        assert!(copy.contains("remove"));
    }

    #[test]
    fn remove_is_only_for_other_devices_in_synced_workspaces() {
        assert!(!can_remove_device(Some(WorkspaceScope::Local), false));
        assert!(!can_remove_device(Some(WorkspaceScope::Synced), true));
        assert!(can_remove_device(Some(WorkspaceScope::Synced), false));
        assert!(can_remove_device(Some(WorkspaceScope::Development), false));
        assert!(!can_remove_device(None, false));
    }

    #[test]
    fn restart_resolves_on_reported_version_or_times_out() {
        let since = Instant::now();
        let phase = DeviceUpdate::Restarting {
            target: "0.3.17".into(),
            since,
        };
        assert_eq!(restart_resolved(&phase, Some("0.3.16"), since), None);
        assert_eq!(
            restart_resolved(&phase, Some("0.3.17"), since),
            Some(DeviceUpdate::UpToDate)
        );
        let late = since + RESTART_TIMEOUT;
        assert!(matches!(
            restart_resolved(&phase, Some("0.3.16"), late),
            Some(DeviceUpdate::Failed { busy: false, .. })
        ));
        assert_eq!(
            restart_resolved(&DeviceUpdate::UpToDate, Some("0.3.17"), late),
            None
        );
        assert_eq!(
            update_badge(&DeviceUpdate::Available("0.3.17".into())),
            "0.3.17 available"
        );
        assert!(is_busy_error(
            "busy: this device has active runs or open terminals"
        ));
        assert!(!is_busy_error("Check timed out"));
    }

    #[test]
    fn delete_copy_explains_local_only() {
        let copy = delete_device_copy("vps");
        assert!(copy.contains("vps"));
        assert!(copy.contains("local-only"));
        assert!(copy.contains("re-pair"));
        assert!(!copy.contains("deletes"));
    }
}
