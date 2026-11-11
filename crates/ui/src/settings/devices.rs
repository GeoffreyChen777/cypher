//! Settings → Devices: one list of the account's machines. Each row joins
//! the device registry (name, platform, version, last-seen, presence dot)
//! with the trust group (key fingerprint, and whether the device has full
//! control or is receive-only), so paired devices that never registered —
//! the iPhone — show up too. Pairing flows (show a code and approve, or
//! enter one to join) sit above the list (`device_pairing`), and the
//! group's "Only paired devices" policy below it.
//!
//! Row actions: Update; the access pill (Full control / Receive-only) on
//! other paired devices; and a menu with Rename (Mutate renameDevice), Copy
//! device ID, Unpair (RemoveTrustedDevice: the machine stays signed in, its
//! keys stop being trusted) and Remove (Mutate deleteDevice: the machine
//! signs out of sync and continues in local-only mode, and when it is paired
//! this device also unpairs it). This device cannot be removed from here.
//! On a receive-only device the list is read-only beyond this device itself.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, MouseButton, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use cypher_engine::device_trust::{TrustMember, TrustStatus};
use cypher_proto::{Device, WorkspaceScope};
use cypher_rpc::methods;

use crate::kit::popover;
use crate::kit::theme::Theme;
use crate::settings::widgets;
use crate::state::{AppState, device_online};
use crate::widgets::text_input::{TextInput, TextInputEvent};

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
        Some(WorkspaceScope::Local) => "Manage device details stored locally on this device.",
        Some(WorkspaceScope::Synced | WorkspaceScope::Development) => {
            "Rename, update or remove your machines, and choose which ones can control the others. Paired devices prove who they are with their own keys, so a stolen sign-in or a compromised server can't control them."
        }
        None => "Manage device names for your account.",
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

/// Confirm-dialog body; `paired` = the machine is also in this device's
/// trust group, so Remove unpairs it too. Pure so the wording is unit-tested.
pub fn delete_device_copy(name: &str, paired: bool) -> String {
    if paired {
        format!(
            "Removing \u{201c}{name}\u{201d} signs that machine out of this account and unpairs it, so your other devices stop trusting its keys. It will continue in local-only mode. To use it again, sign in there and re-pair it with a new code."
        )
    } else {
        format!(
            "Removing \u{201c}{name}\u{201d} signs that machine out of this account. It will continue in local-only mode. Signing in again there will re-pair it."
        )
    }
}

/// One row of the list: a device from the registry, a member of the trust
/// group, or both (matched by device id).
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRow {
    pub id: String,
    pub device: Option<Device>,
    pub member: Option<TrustMember>,
}

impl DeviceRow {
    /// The registry's name (renamable) over the name the device paired with.
    pub fn name(&self) -> &str {
        match (&self.device, &self.member) {
            (Some(device), _) => &device.name,
            (None, Some(member)) => &member.name,
            (None, None) => &self.id,
        }
    }
}

/// Registry devices in registry order, then paired devices the registry
/// doesn't list. `trust` lists members only while this device is paired.
/// Pure.
pub fn merge_rows(devices: Vec<Device>, trust: Option<&TrustStatus>) -> Vec<DeviceRow> {
    let members: &[TrustMember] = trust.map_or(&[], |t| t.members.as_slice());
    let mut rows: Vec<DeviceRow> = devices
        .into_iter()
        .map(|device| DeviceRow {
            id: device.id.clone(),
            member: members.iter().find(|m| m.id == device.id).cloned(),
            device: Some(device),
        })
        .collect();
    for member in members {
        if !rows.iter().any(|row| row.id == member.id) {
            rows.push(DeviceRow {
                id: member.id.clone(),
                device: None,
                member: Some(member.clone()),
            });
        }
    }
    rows
}

/// What a device may do to the others, as the trust group says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    FullControl,
    ReceiveOnly,
    NotPaired,
}

/// A row's access pill, or `None` while this device can't tell (pairing
/// unavailable, or this device isn't paired, so it holds no trusted list).
/// Pure.
pub fn row_access(row: &DeviceRow, trust: Option<&TrustStatus>) -> Option<Access> {
    let trust = trust?;
    if trust.state != cypher_engine::device_trust::TrustState::Paired {
        return None;
    }
    Some(match &row.member {
        Some(member) if member.receive_only => Access::ReceiveOnly,
        Some(_) => Access::FullControl,
        None => Access::NotPaired,
    })
}

pub fn access_label(access: Access) -> &'static str {
    match access {
        Access::FullControl => "Full control",
        Access::ReceiveOnly => "Receive-only",
        Access::NotPaired => "Not paired",
    }
}

/// Whether this device has full control in its group. Pure.
fn controls(trust: Option<&TrustStatus>) -> bool {
    trust.is_some_and(|t| {
        t.state == cypher_engine::device_trust::TrustState::Paired && !t.receive_only
    })
}

/// This device may change `row`'s access: `row` is another member, and this
/// device has full control. Pure.
pub fn can_set_access(row: &DeviceRow, trust: Option<&TrustStatus>) -> bool {
    controls(trust) && row.member.as_ref().is_some_and(|m| !m.is_self)
}

/// Unpairing is the same decision as access. Pure.
pub fn can_unpair(row: &DeviceRow, trust: Option<&TrustStatus>) -> bool {
    can_set_access(row, trust)
}

/// This device is a receive-only member: beyond itself, the list is
/// read-only. Pure.
pub fn local_receive_only(trust: Option<&TrustStatus>) -> bool {
    trust.is_some_and(|t| t.receive_only)
}

/// Whether removing `device_id` should also unpair it: it is another member
/// of the group this device belongs to. Pure.
pub fn unpair_on_remove(status: Option<&TrustStatus>, device_id: &str) -> bool {
    status.is_some_and(|s| {
        s.state == cypher_engine::device_trust::TrustState::Paired
            && s.device_id != device_id
            && s.members.iter().any(|m| m.id == device_id)
    })
}

/// Which of a row's menus is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    Access,
    More,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowMenu {
    device_id: String,
    kind: MenuKind,
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
/// version; after [`RESTART_TIMEOUT`] without it, the row says so.
pub const RESTART_TIMEOUT: Duration = Duration::from_secs(240);

/// Follow a device's update phase from the version its registry row reports.
/// The page checks the fleet once, so a device that updated itself afterwards
/// (Linux auto-update, `cypher update`, another desktop) must not keep
/// offering the release it already runs. Pure.
pub fn registry_resolved(
    phase: &DeviceUpdate,
    device_version: Option<&str>,
    now: Instant,
) -> Option<DeviceUpdate> {
    let reached = |target: &str| {
        device_version.is_some_and(|v| v == target || cypher_update::version_newer(v, target))
    };
    let (target, since) = match phase {
        DeviceUpdate::Available(latest) => {
            return reached(latest).then_some(DeviceUpdate::UpToDate);
        }
        DeviceUpdate::Restarting { target, since } => (target, since),
        _ => return None,
    };
    if reached(target) {
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
    input: Entity<TextInput>,
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
    /// A row's access or actions menu.
    menu: popover::Popup<RowMenu>,
    /// Pairing: this device's place in the account's trust group.
    pairing: Entity<crate::settings::device_pairing::PairingSection>,
    _observe_pairing: Subscription,
    _observe: Subscription,
}

impl DevicesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, state, cx| {
            this.reconcile_registry(&state, cx);
            cx.notify()
        });
        let pairing =
            cx.new(|cx| crate::settings::device_pairing::PairingSection::new(state.clone(), cx));
        // Row badges follow the pairing status.
        let observe_pairing = cx.observe(&pairing, |_, _, cx| cx.notify());
        Self {
            state,
            pairing,
            _observe_pairing: observe_pairing,
            rename: None,
            delete_confirm: None,
            copied: None,
            error: None,
            task: None,
            copy_task: None,
            updates: HashMap::new(),
            update_tasks: Vec::new(),
            auto_checked: false,
            menu: popover::Popup::default(),
            _observe: observe,
        }
    }

    fn open_rename(&mut self, device_id: String, current: String, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        let input = cx.new(|cx| TextInput::new("Device name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
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
        let unpair = unpair_on_remove(self.pairing.read(cx).status(), &device_id);
        let params = serde_json::json!({
            "op": "deleteDevice",
            "deviceId": device_id,
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let removed = engine.client().call(methods::MUTATE, params).await;
            // Unpair through this device's own engine: the trust RPCs are
            // local-only, so the signature is always this device's decision.
            let unpaired = match (&removed, unpair) {
                (Ok(_), true) => Some(
                    engine
                        .client()
                        .call(
                            methods::REMOVE_TRUSTED_DEVICE,
                            serde_json::json!({ "deviceId": device_id }),
                        )
                        .await,
                ),
                _ => None,
            };
            this.update(cx, |page, cx| {
                match (removed, unpaired) {
                    (Err(err), _) => page.error = Some(format!("Remove failed: {err}").into()),
                    (Ok(_), Some(Err(err))) => page.error = Some(
                        format!(
                            "Removed, but it is still paired: {err}. Unpair it from its row's menu."
                        )
                        .into(),
                    ),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- fleet updates ----

    /// Available and Restarting rows resolve from the registry: the device's
    /// engine writes its version on boot.
    fn reconcile_registry(&mut self, state: &Entity<AppState>, cx: &App) {
        let now = Instant::now();
        let versions: HashMap<String, Option<String>> = state
            .read(cx)
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.version.clone()))
            .collect();
        for (id, phase) in self.updates.iter_mut() {
            if let Some(next) =
                registry_resolved(phase, versions.get(id).and_then(|v| v.as_deref()), now)
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
        // A receive-only device can't reach the others.
        let only_local = local_receive_only(self.pairing.read(cx).status());
        let state = self.state.read(cx);
        let ids: Vec<String> = state
            .devices
            .iter()
            .filter(|d| d.platform != "ios" && d.platform != "android")
            .filter(|d| device_online(d.last_seen_at, now))
            .filter(|d| !only_local || state.local_device_id.as_deref() == Some(d.id.as_str()))
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
        let paired = unpair_on_remove(self.pairing.read(cx).status(), &device_id);
        let copy = delete_device_copy(&name, paired);
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

impl Render for DevicesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        if !self.auto_checked && !devices.is_empty() && self.state.read(cx).engine().is_some() {
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

        let synced = matches!(
            workspace_scope,
            Some(WorkspaceScope::Synced | WorkspaceScope::Development)
        );
        let (trust, pairing_busy) = {
            let pairing = self.pairing.read(cx);
            (pairing.status().cloned(), pairing.busy())
        };
        let trust = trust.filter(|_| synced);
        let rows = merge_rows(devices, trust.as_ref());
        let count = rows.len();
        let ctx = DeviceRowContext {
            now,
            local_id,
            copied,
            workspace_scope,
            updates: &updates,
            trust: trust.as_ref(),
            busy: pairing_busy,
        };
        let rows: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(ix, row)| self.device_row(ix, row, &ctx, &theme, cx))
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
        // Add a device: a member with full control, nothing in progress.
        let can_add = controls(trust.as_ref())
            && trust.as_ref().is_some_and(|t| t.pairing.is_none())
            && !pairing_busy;

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
                    .when(count > 0 || can_add, |el| {
                        el.child(page_toolbar(checking, available_count, can_add, &theme, cx))
                    })
                    .when(synced, |el| el.child(self.pairing.clone()))
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
                    .child(card.mt(px(16.0)))
                    .when_some(
                        trust
                            .as_ref()
                            .filter(|t| t.state == cypher_engine::device_trust::TrustState::Paired),
                        |el, trust| el.child(self.policy_row(trust, pairing_busy, &theme, cx)),
                    ),
            )
            .when_some(dialog, |el, dialog| el.child(dialog))
            .when_some(delete_dialog, |el, dialog| el.child(dialog))
    }
}

impl DevicesPage {
    /// One device row: identity tile, name over the meta line, the update and
    /// access pills, Update, and the actions menu.
    fn device_row(
        &self,
        ix: usize,
        row: DeviceRow,
        ctx: &DeviceRowContext<'_>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let DeviceRowContext {
            now,
            local_id,
            copied,
            workspace_scope,
            updates,
            trust,
            busy,
        } = ctx;
        let (now, workspace_scope, trust) = (*now, *workspace_scope, *trust);
        let device = row.device.as_ref();
        let online = device.is_some_and(|d| device_online(d.last_seen_at, now));
        let is_local = local_id.as_deref() == Some(row.id.as_str());
        let read_only = local_receive_only(trust) && !is_local;
        // A receive-only device can't reach the others: nothing to report.
        let phase = updates.get(&row.id).cloned().filter(|_| !read_only);
        let update_action: Option<(&'static str, bool)> = match &phase {
            Some(DeviceUpdate::Available(_)) => Some(("Update", false)),
            Some(DeviceUpdate::Failed { busy: true, .. }) => Some(("Update anyway", true)),
            Some(DeviceUpdate::Failed { busy: false, .. }) => Some(("Retry", false)),
            _ => None,
        };
        let platform_icon = match device.map(|d| d.platform.as_str()) {
            Some("macos" | "darwin") => crate::kit::icons::LAPTOP,
            Some("web") => crate::kit::icons::GLOBAL,
            Some("ios" | "android") => crate::kit::icons::SMARTPHONE,
            Some(_) => crate::kit::icons::MONITOR,
            // Paired but not in the registry: the iPhone app pairs without
            // registering, so its name is the best hint there is.
            None if row.name().starts_with("iPhone") || row.name().starts_with("iPad") => {
                crate::kit::icons::SMARTPHONE
            }
            None => crate::kit::icons::KEY_MINIMALISTIC,
        };
        let tile = device_tile(device.map(|_| online), platform_icon, theme);
        let id_copied = copied.as_deref() == Some(row.id.as_str());
        let meta = device_meta(&row, online, now, id_copied, theme);
        // Update state rides a right-aligned pill, never the meta line: a
        // long status pushed the meta onto its own row and rows stopped
        // lining up.
        let status_pill = phase.as_ref().map(|phase| update_status_pill(phase, theme));
        let access = row_access(&row, trust);
        let access_pill = access.map(|access| {
            self.access_pill(
                ix,
                &row,
                access,
                can_set_access(&row, trust) && !busy,
                theme,
                cx,
            )
        });
        let local_label = is_local.then_some(if workspace_scope == Some(WorkspaceScope::Local) {
            "Local only"
        } else {
            "This device"
        });
        let update_id = row.id.clone();

        widgets::card_row(theme, ix == 0)
            .child(tile)
            .child(
                div()
                    .flex_1()
                    .min_w(px(220.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_baseline()
                            .gap(px(8.0))
                            .min_w_0()
                            .child(widgets::row_title(theme, row.name().to_string()))
                            .when_some(local_label, |el, label| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(11.0))
                                        .text_color(theme.text_muted.opacity(0.8))
                                        .child(label),
                                )
                            }),
                    )
                    .child(widgets::meta_line(theme, meta)),
            )
            .when_some(status_pill, |el, pill| el.child(pill))
            .when_some(access_pill, |el, pill| el.child(pill))
            .when_some(update_action, |el, (label, force)| {
                let retry_check = label == "Retry";
                let id = update_id.clone();
                el.child(
                    widgets::ghost_action(theme)
                        .id(("device-update", ix))
                        .text_color(theme.accent)
                        .hover(|s| s.bg(theme.accent.opacity(0.10)).text_color(theme.accent))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if retry_check {
                                this.check_device(id.clone(), cx);
                            } else {
                                this.apply_device(id.clone(), force, cx);
                            }
                        }))
                        .child(
                            crate::kit::icons::icon(crate::kit::icons::ARCHIVE_UP_MINIMALISTIC)
                                .size(px(14.0))
                                .text_color(theme.accent),
                        )
                        .child(SharedString::from(label)),
                )
            })
            .child(self.more_button(ix, &row, ctx, theme, cx))
            .into_any_element()
    }

    /// The access pill; a menu trigger where this device may change it.
    fn access_pill(
        &self,
        ix: usize,
        row: &DeviceRow,
        access: Access,
        editable: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (tone, text, glyph) = match access {
            Access::FullControl => (
                theme.success,
                theme.success_muted,
                Some(crate::kit::icons::SHIELD_CHECK),
            ),
            Access::ReceiveOnly => (
                theme.warning,
                theme.warning_muted,
                Some(crate::kit::icons::SHIELD_KEYHOLE),
            ),
            Access::NotPaired => (theme.text_muted, theme.text_muted, None),
        };
        let pill = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(2.0))
            .rounded_full()
            .text_size(px(10.5))
            .text_color(text.opacity(0.9))
            .map(|el| match access {
                Access::NotPaired => el.border_1().border_color(theme.border),
                _ => el.bg(tone.opacity(0.12)),
            })
            .when_some(glyph, |el, glyph| {
                el.child(
                    crate::kit::icons::icon(glyph)
                        .size(px(11.0))
                        .text_color(text.opacity(0.9)),
                )
            })
            .child(SharedString::from(access_label(access)));
        if !editable {
            return pill.into_any_element();
        }
        let open = self
            .menu
            .get()
            .is_some_and(|m| m.device_id == row.id && m.kind == MenuKind::Access);
        let device_id = row.id.clone();
        let press_id = row.id.clone();
        let mut trigger = pill
            .id(("device-access", ix))
            .relative()
            .cursor_pointer()
            .hover(|s| s.bg(tone.opacity(0.2)))
            .when(open, |el| el.bg(tone.opacity(0.2)))
            .child(
                crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_DOWN)
                    .size(px(10.0))
                    .text_color(text.opacity(0.7)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    this.menu.note_trigger_press_matching(|m| {
                        m.device_id == press_id && m.kind == MenuKind::Access
                    });
                }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_menu(device_id.clone(), MenuKind::Access, cx)
            }));
        if open && let Some(member) = row.member.clone() {
            trigger = trigger.child(self.render_access_menu(member, theme, cx));
        }
        trigger.into_any_element()
    }

    /// The row's `•••` button and its menu.
    fn more_button(
        &self,
        ix: usize,
        row: &DeviceRow,
        ctx: &DeviceRowContext<'_>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let items = self.more_items(row, ctx);
        let open = self
            .menu
            .get()
            .is_some_and(|m| m.device_id == row.id && m.kind == MenuKind::More);
        let device_id = row.id.clone();
        let press_id = row.id.clone();
        let mut button = div()
            .id(("device-more", ix))
            .relative()
            .flex_none()
            .w(px(32.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .text_size(px(10.0))
            .text_color(theme.text_muted)
            .when(open, |el| el.bg(crate::kit::theme::ink(0.06)))
            .when(items.is_empty(), |el| el.opacity(0.3))
            .when(!items.is_empty(), |el| {
                el.cursor_pointer()
                    .hover(|s| widgets::ghost_hover(theme, s))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, _| {
                            this.menu.note_trigger_press_matching(|m| {
                                m.device_id == press_id && m.kind == MenuKind::More
                            });
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_menu(device_id.clone(), MenuKind::More, cx)
                    }))
            })
            .child(SharedString::from("•••"));
        if open && !items.is_empty() {
            button = button.child(self.render_more_menu(row.clone(), items, theme, cx));
        }
        button.into_any_element()
    }

    /// What the row's menu offers, in order.
    fn more_items(&self, row: &DeviceRow, ctx: &DeviceRowContext<'_>) -> Vec<MoreItem> {
        let is_local = ctx.local_id.as_deref() == Some(row.id.as_str());
        let read_only = local_receive_only(ctx.trust) && !is_local;
        let mut items = Vec::new();
        if row.device.is_some() && !read_only {
            items.push(MoreItem::Rename);
        }
        items.push(MoreItem::CopyId);
        if can_unpair(row, ctx.trust) {
            items.push(MoreItem::Unpair);
        }
        if row.device.is_some() && can_remove_device(ctx.workspace_scope, is_local) && !read_only {
            items.push(MoreItem::Remove);
        }
        items
    }

    fn toggle_menu(&mut self, device_id: String, kind: MenuKind, cx: &mut Context<Self>) {
        if self.menu.take_press_was_open() {
            self.close_menu(cx);
            return;
        }
        self.menu.open(RowMenu { device_id, kind });
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.begin_close() {
            popover::reap_popup(cx, |page: &mut Self| &mut page.menu);
            cx.notify();
        }
    }

    fn render_access_menu(
        &self,
        member: TrustMember,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let closing = self.menu.closing_since();
        let mut card = popover::popover_card(theme)
            .id("device-access-menu")
            .role(gpui::Role::Menu)
            .aria_label("Device access")
            .w(px(280.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));
        let choices = [
            (
                false,
                "Full control",
                "Can control your other devices, add or remove devices, and change these settings.",
                crate::kit::icons::SHIELD_CHECK,
            ),
            (
                true,
                "Receive-only",
                "Your other devices can control it and send it messages. It can't control them.",
                crate::kit::icons::SHIELD_KEYHOLE,
            ),
        ];
        for (index, (receive_only, title, caption, glyph)) in choices.into_iter().enumerate() {
            let current = member.receive_only == receive_only;
            let choice = member.clone();
            card = card.child(
                popover::menu_row(theme, false, format!("device-access-choice-{index}"))
                    .id(("device-access-choice", index))
                    .role(gpui::Role::MenuItem)
                    .aria_label(title)
                    .items_start()
                    .py(px(8.0))
                    .when(closing.is_none(), |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            let member = choice.clone();
                            this.pairing.update(cx, |pairing, cx| {
                                pairing.set_access(member, receive_only, cx)
                            });
                        }))
                    })
                    .child(
                        crate::kit::icons::icon(glyph)
                            .size(px(15.0))
                            .mt(px(1.0))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(SharedString::from(title))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(caption)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.0))
                            .mt(px(1.0))
                            .when(current, |el| {
                                el.child(
                                    crate::kit::icons::icon(crate::kit::icons::CHECK)
                                        .size(px(14.0))
                                        .text_color(theme.text),
                                )
                            }),
                    ),
            );
        }
        popover::anchored_menu_below_end("device-access-layer", card.into_any_element(), closing)
    }

    fn render_more_menu(
        &self,
        row: DeviceRow,
        items: Vec<MoreItem>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let closing = self.menu.closing_since();
        let mut card = popover::popover_card(theme)
            .id("device-actions-menu")
            .role(gpui::Role::Menu)
            .aria_label("Device actions")
            .w(px(200.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));
        let mut separated = false;
        for (index, item) in items.into_iter().enumerate() {
            let destructive = matches!(item, MoreItem::Unpair | MoreItem::Remove);
            // The destructive items form their own group.
            let separate = destructive && index > 0 && !separated;
            separated |= destructive;
            let (label, glyph) = item.label();
            let row = row.clone();
            card = card
                .when(separate, |el| el.child(popover::menu_separator()))
                .child(
                    popover::menu_row(theme, false, format!("device-action-{index}"))
                        .id(("device-action", index))
                        .role(gpui::Role::MenuItem)
                        .aria_label(label)
                        .min_h(px(32.0))
                        .when(destructive, |el| el.text_color(theme.danger_muted))
                        .when(closing.is_none(), |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                this.more_action(item, row.clone(), cx);
                            }))
                        })
                        .child(crate::kit::icons::icon(glyph).size(px(15.0)).text_color(
                            if destructive {
                                theme.danger_muted
                            } else {
                                theme.text_muted
                            },
                        ))
                        .child(SharedString::from(label)),
                );
        }
        popover::anchored_menu_below_end("device-actions-layer", card.into_any_element(), closing)
    }

    fn more_action(&mut self, item: MoreItem, row: DeviceRow, cx: &mut Context<Self>) {
        match item {
            MoreItem::Rename => self.open_rename(row.id.clone(), row.name().to_string(), cx),
            MoreItem::CopyId => self.copy_id(row.id, cx),
            MoreItem::Unpair => {
                if let Some(member) = row.member {
                    self.pairing
                        .update(cx, |pairing, cx| pairing.ask_unpair(member, cx));
                }
            }
            MoreItem::Remove => self.open_delete(row.id, cx),
        }
    }

    /// "Only paired devices", below the list.
    fn policy_row(
        &self,
        trust: &TrustStatus,
        busy: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let require = trust.require_paired;
        let lock = crate::settings::device_pairing::policy_lock_copy(trust);
        let enabled = lock.is_none() && !busy;
        div()
            .mt(px(20.0))
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
                            .when(!enabled, |el| el.opacity(0.5))
                            .when(enabled, |el| {
                                el.cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pairing.update(cx, |pairing, cx| {
                                            pairing.set_require_paired(!require, cx)
                                        })
                                    }))
                            }),
                    )
                    .child(widgets::field_label(theme, "Only paired devices")),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme.text_muted)
                    .child(crate::settings::device_pairing::policy_copy()),
            )
            .when_some(lock, |el, lock| {
                el.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted.opacity(0.7))
                        .child(lock),
                )
            })
    }
}

/// One entry in a row's actions menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoreItem {
    Rename,
    CopyId,
    Unpair,
    Remove,
}

impl MoreItem {
    fn label(self) -> (&'static str, &'static str) {
        match self {
            MoreItem::Rename => ("Rename…", crate::kit::icons::PEN),
            MoreItem::CopyId => ("Copy device ID", crate::kit::icons::COPY),
            MoreItem::Unpair => ("Unpair…", crate::kit::icons::SHIELD_CROSS),
            MoreItem::Remove => (
                "Remove from account…",
                crate::kit::icons::TRASH_BIN_MINIMALISTIC,
            ),
        }
    }
}

/// One quiet meta line: platform · version · (offline: last seen) · added ·
/// key, and the name the device paired with when it was renamed since.
fn device_meta(
    row: &DeviceRow,
    online: bool,
    now: DateTime<Utc>,
    id_copied: bool,
    theme: &Theme,
) -> Vec<AnyElement> {
    let text = |copy: String| div().child(SharedString::from(copy)).into_any_element();
    let mut meta: Vec<AnyElement> = Vec::new();
    if let Some(device) = &row.device {
        meta.push(text(platform_label(&device.platform).to_string()));
        if let Some(version) = device.version.as_deref().filter(|v| !v.is_empty()) {
            meta.push(text(format!("v{version}")));
        }
        if !online {
            meta.push(text(format!(
                "Last seen {}",
                format_last_seen(device.last_seen_at, now)
            )));
        }
        // "Added {time ago}" — always present (zeron settings.devices.tsx).
        if let Some(created) = device.created_at {
            meta.push(text(format!(
                "Added {}",
                format_last_seen(Some(created), now)
            )));
        }
    }
    if let Some(member) = &row.member {
        if row.device.as_ref().is_some_and(|d| d.name != member.name) {
            meta.push(text(format!("paired as {}", member.name)));
        }
        meta.push(
            crate::settings::device_pairing::key_label(theme, &member.fingerprint)
                .into_any_element(),
        );
    }
    if id_copied {
        meta.push(
            div()
                .text_color(theme.success_muted.opacity(0.9))
                .child(SharedString::from("ID copied"))
                .into_any_element(),
        );
    }
    meta
}

/// What every device row reads from the page.
struct DeviceRowContext<'a> {
    now: DateTime<Utc>,
    local_id: Option<String>,
    copied: Option<String>,
    workspace_scope: Option<WorkspaceScope>,
    updates: &'a HashMap<String, DeviceUpdate>,
    trust: Option<&'a TrustStatus>,
    /// A pairing call is in flight.
    busy: bool,
}

/// The device's identity tile. Presence lives ON the tile: a corner dot
/// (emerald online with a soft glow, faint offline), ringed by the card
/// tone so it "cuts" the tile — zeron settings.devices.tsx
/// `border-2 border-[var(--card)]` +
/// `shadow-[0_0_6px_rgba(52,211,153,0.55)]`. `None`: no registry record,
/// so no presence to show.
fn device_tile(online: Option<bool>, platform_icon: &'static str, theme: &Theme) -> gpui::Div {
    let emerald = theme.success; // emerald-400
    widgets::row_tile(theme, platform_icon)
        .relative()
        .when_some(online, |el, online| {
            el.child(
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
                    .when(!online, |el| el.bg(crate::kit::theme::ink(0.22))),
            )
        })
}

/// The right-aligned pill naming a device's update state.
fn update_status_pill(phase: &DeviceUpdate, theme: &Theme) -> gpui::Div {
    let tone = match phase {
        DeviceUpdate::Available(_) => theme.accent,
        DeviceUpdate::Failed { .. } => theme.danger,
        DeviceUpdate::UpToDate => theme.success_muted,
        _ => theme.text_muted,
    };
    // Shrinks (truncating) before the device name does.
    div()
        .flex_shrink(1.0)
        .min_w(px(48.0))
        .max_w(px(200.0))
        .px(px(8.0))
        .py(px(2.0))
        .rounded_full()
        .bg(tone.opacity(0.10))
        .text_size(px(10.5))
        .text_color(tone)
        .truncate()
        .child(SharedString::from(update_badge(phase)))
}

/// The toolbar over the list. Fleet updates: check every online device,
/// update the ones with a newer release (each device applies its own release
/// and restarts itself; iOS updates through TestFlight and is left out).
/// Add a device sits on the trailing edge.
fn page_toolbar(
    checking: bool,
    available_count: usize,
    can_add: bool,
    theme: &Theme,
    cx: &mut Context<DevicesPage>,
) -> gpui::Div {
    div()
        .mt(px(12.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            widgets::ghost_action(theme)
                .id("devices-check-updates")
                .hover(|s| widgets::ghost_hover(theme, s))
                .when(checking, |el| el.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.check_all(cx)))
                .child(
                    crate::kit::icons::icon(crate::kit::icons::REFRESH)
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
                widgets::ghost_action(theme)
                    .id("devices-update-all")
                    .text_color(theme.accent)
                    .hover(|s| s.bg(theme.accent.opacity(0.10)).text_color(theme.accent))
                    .on_click(cx.listener(|this, _, _, cx| this.apply_all(cx)))
                    .child(
                        crate::kit::icons::icon(crate::kit::icons::ARCHIVE_UP_MINIMALISTIC)
                            .size(px(14.0))
                            .text_color(theme.accent),
                    )
                    .child(SharedString::from(format!(
                        "Update all ({available_count})"
                    ))),
            )
        })
        .child(div().flex_1())
        .when(can_add, |el| {
            el.child(
                widgets::ghost_action(theme)
                    .id("pairing-add")
                    .text_color(theme.text)
                    .border_1()
                    .border_color(theme.border)
                    .hover(|s| widgets::ghost_hover(theme, s))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pairing
                            .update(cx, |pairing, cx| pairing.start_invite(cx))
                    }))
                    .child(
                        crate::kit::icons::icon(crate::kit::icons::PLUS)
                            .size(px(14.0))
                            .text_color(theme.text),
                    )
                    .child(SharedString::from("Add a device")),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use cypher_engine::device_trust::TrustState;

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
        assert!(copy.contains("locally on this device"));
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
        assert_eq!(registry_resolved(&phase, Some("0.3.16"), since), None);
        assert_eq!(
            registry_resolved(&phase, Some("0.3.17"), since),
            Some(DeviceUpdate::UpToDate)
        );
        assert_eq!(
            registry_resolved(&phase, Some("0.3.18"), since),
            Some(DeviceUpdate::UpToDate)
        );
        let late = since + RESTART_TIMEOUT;
        assert!(matches!(
            registry_resolved(&phase, Some("0.3.16"), late),
            Some(DeviceUpdate::Failed { busy: false, .. })
        ));
        assert_eq!(
            registry_resolved(&DeviceUpdate::UpToDate, Some("0.3.17"), late),
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
    fn available_clears_once_the_device_reports_the_release() {
        let now = Instant::now();
        let phase = DeviceUpdate::Available("0.4.1".into());
        assert_eq!(registry_resolved(&phase, Some("0.4.0"), now), None);
        assert_eq!(registry_resolved(&phase, None, now), None);
        assert_eq!(
            registry_resolved(&phase, Some("0.4.1"), now),
            Some(DeviceUpdate::UpToDate)
        );
        assert_eq!(
            registry_resolved(&phase, Some("0.4.2"), now),
            Some(DeviceUpdate::UpToDate)
        );
    }

    #[test]
    fn delete_copy_explains_local_only() {
        for paired in [false, true] {
            let copy = delete_device_copy("vps", paired);
            assert!(copy.contains("vps"));
            assert!(copy.contains("local-only"));
            assert!(copy.contains("re-pair"));
            assert!(!copy.contains("deletes"));
        }
        assert!(delete_device_copy("vps", true).contains("unpairs"));
        assert!(!delete_device_copy("vps", false).contains("unpairs"));
    }

    fn trust(state: TrustState, members: &[&str]) -> TrustStatus {
        TrustStatus {
            device_id: "me".into(),
            fingerprint: "00".into(),
            state,
            members: members
                .iter()
                .map(|id| TrustMember {
                    id: (*id).into(),
                    name: (*id).into(),
                    fingerprint: "00".into(),
                    is_self: *id == "me",
                    receive_only: false,
                })
                .collect(),
            require_paired: false,
            receive_only: false,
            pairing: None,
            conflict: false,
            error: None,
        }
    }

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            platform: "macos".into(),
            last_seen_at: None,
            created_at: None,
            version: None,
        }
    }

    #[test]
    fn one_list_joins_the_registry_and_the_trust_group() {
        let mut status = trust(TrustState::Paired, &["me", "mac", "phone"]);
        status.members[1].name = "old name".into();
        let rows = merge_rows(
            vec![
                device("mac", "Mac mini"),
                device("me", "MacBook"),
                device("vps", "vps"),
            ],
            Some(&status),
        );
        let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
        // Registry order first, then members the registry doesn't list.
        assert_eq!(ids, ["mac", "me", "vps", "phone"]);
        assert_eq!(rows[0].name(), "Mac mini", "the registry name wins");
        assert!(rows[0].member.is_some());
        assert!(rows[2].member.is_none());
        assert!(rows[3].device.is_none());
        assert_eq!(rows[3].name(), "phone");
        // Without a trusted list, only the registry.
        let rows = merge_rows(vec![device("mac", "Mac mini")], None);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].member.is_none());
    }

    #[test]
    fn rows_show_access_only_when_this_device_can_tell() {
        let mut paired = trust(TrustState::Paired, &["me", "mac"]);
        paired.members[1].receive_only = true;
        let rows = merge_rows(
            vec![
                device("me", "me"),
                device("mac", "mac"),
                device("vps", "vps"),
            ],
            Some(&paired),
        );
        assert_eq!(
            row_access(&rows[0], Some(&paired)),
            Some(Access::FullControl)
        );
        assert_eq!(
            row_access(&rows[1], Some(&paired)),
            Some(Access::ReceiveOnly)
        );
        assert_eq!(row_access(&rows[2], Some(&paired)), Some(Access::NotPaired));
        // Without a trusted list of its own, this device can't vouch either way.
        for state in [
            TrustState::Unpaired,
            TrustState::Removed,
            TrustState::Unknown,
        ] {
            assert_eq!(row_access(&rows[1], Some(&trust(state, &[]))), None);
        }
        assert_eq!(row_access(&rows[1], None), None);
        assert_eq!(access_label(Access::ReceiveOnly), "Receive-only");
    }

    #[test]
    fn only_a_device_with_full_control_changes_the_others() {
        let mut paired = trust(TrustState::Paired, &["me", "mac"]);
        let rows = merge_rows(
            vec![
                device("me", "me"),
                device("mac", "mac"),
                device("vps", "vps"),
            ],
            Some(&paired),
        );
        assert!(!can_set_access(&rows[0], Some(&paired)), "not itself");
        assert!(can_set_access(&rows[1], Some(&paired)));
        assert!(
            !can_set_access(&rows[2], Some(&paired)),
            "not an unpaired device"
        );
        assert!(can_unpair(&rows[1], Some(&paired)));
        assert!(!local_receive_only(Some(&paired)));
        paired.receive_only = true;
        paired.members[0].receive_only = true;
        assert!(local_receive_only(Some(&paired)));
        assert!(!can_set_access(&rows[1], Some(&paired)));
        assert!(!can_unpair(&rows[1], Some(&paired)));
    }

    #[test]
    fn removing_a_paired_machine_unpairs_it_but_never_this_device() {
        let paired = trust(TrustState::Paired, &["me", "mac"]);
        assert!(unpair_on_remove(Some(&paired), "mac"));
        assert!(!unpair_on_remove(Some(&paired), "me"));
        assert!(!unpair_on_remove(Some(&paired), "vps"));
        assert!(!unpair_on_remove(
            Some(&trust(TrustState::Unpaired, &[])),
            "mac"
        ));
        assert!(!unpair_on_remove(None, "mac"));
    }
}
