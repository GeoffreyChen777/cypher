//! Sidebar mutations (rename, archive, pin, delete, the view menu) and the
//! account actions behind the user menu (sign in/out, the sync switch).

use super::*;
use crate::prefs::sidebar_time;
use gpui::{Hsla, relative};

/// Drag marker for the view menu's last-active slider.
struct SidebarTimeDrag;

/// The time field's text for a window: empty (the "All" placeholder) when
/// there is none.
fn time_field_text(mins: Option<u32>) -> String {
    mins.map(|m| sidebar_time::format(Some(m)))
        .unwrap_or_default()
}

impl Shell {
    /// Fire a Mutate op; failures surface in the sidebar notice strip.
    pub(super) fn mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar.notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.mutate_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine.client().call(methods::MUTATE, params).await {
                this.update(cx, |shell, cx| {
                    shell.sidebar.notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    pub(super) fn open_rename_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.close_chat_menu(cx);
        let current = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id)
            .and_then(|c| c.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| TextInput::new("Session title", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.submit_rename_chat(cx);
            }
        });
        self.dialogs.rename_chat = Some(RenameChatDialog {
            chat_id,
            input,
            focus_pending: true,
            _events: events,
        });
        cx.notify();
    }

    pub(super) fn submit_rename_chat(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialogs.rename_chat.take() else {
            return;
        };
        let title = dialog.input.read(cx).text().trim().to_string();
        if !title.is_empty() {
            self.mutate(
                serde_json::json!({ "op": "renameChat", "chatId": dialog.chat_id, "title": title }),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn archive_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.set_chat_archived(chat_id, true, cx);
    }

    pub(super) fn set_chat_archived(
        &mut self,
        chat_id: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatArchived", "chatId": chat_id, "archived": archived }),
            cx,
        );
        cx.notify();
    }

    pub(super) fn close_sidebar_view_menu(&mut self, cx: &mut Context<Self>) {
        if self.menus.sidebar_view.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.menus.sidebar_view);
            cx.notify();
        }
    }

    /// The sidebar view menu's current state (persisted in ui-settings).
    pub(super) fn sidebar_view(&self) -> crate::state::SidebarView {
        crate::state::SidebarView {
            // A project window lists one project: a device filter could only
            // empty it.
            device: self
                .settings
                .sidebar_device_filter
                .clone()
                .filter(|_| !self.is_project_window()),
            active_within_mins: self.settings.sidebar_active_within_mins,
            sort: self.settings.sidebar_sort,
            reversed: self.settings.sidebar_sort_reversed,
        }
    }

    /// A new sort starts in its natural direction.
    pub(super) fn set_sidebar_sort(
        &mut self,
        sort: crate::prefs::SidebarSort,
        cx: &mut Context<Self>,
    ) {
        if self.settings.sidebar_sort != sort {
            self.settings.sidebar_sort_reversed = false;
        }
        self.settings.sidebar_sort = sort;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn set_sidebar_descending(&mut self, descending: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_sort_reversed =
            self.settings.sidebar_sort.natural_descending() != descending;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn set_sidebar_device_filter(
        &mut self,
        device: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.settings.sidebar_device_filter = device;
        self.close_sidebar_view_menu(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// The last-active window (minutes; `None` = all). The menu stays open:
    /// the slider and the field adjust it live.
    pub(super) fn set_sidebar_active_within(&mut self, mins: Option<u32>, cx: &mut Context<Self>) {
        let mins = sidebar_time::clamp(mins);
        if self.settings.sidebar_active_within_mins != mins {
            self.settings.sidebar_active_within_mins = mins;
            self.schedule_save(cx);
        }
        cx.notify();
    }

    /// Open the view menu at `position`, its time field showing the current
    /// window.
    pub(super) fn open_sidebar_view_menu(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_space_menu(cx);
        let input = self.sidebar_time_input(cx);
        let text = time_field_text(self.settings.sidebar_active_within_mins);
        input.update(cx, |input, cx| input.set_text(text, cx));
        self.menus.sidebar_view.open(position);
        cx.notify();
    }

    /// The view menu's time field: every readable edit applies live; Enter
    /// tidies the text to its canonical form and closes the menu.
    fn sidebar_time_input(&mut self, cx: &mut Context<Self>) -> Entity<TextInput> {
        if let Some((input, _)) = &self.sidebar.time_input {
            return input.clone();
        }
        let input = cx.new(|cx| TextInput::new("All", cx));
        let events = cx.subscribe(&input, |this: &mut Shell, input, event, cx| match event {
            TextInputEvent::Edited => {
                if let Some(mins) = sidebar_time::parse(input.read(cx).text()) {
                    this.set_sidebar_active_within(mins, cx);
                }
            }
            TextInputEvent::Submitted => {
                let text = time_field_text(this.settings.sidebar_active_within_mins);
                input.update(cx, |input, cx| input.set_text(text, cx));
                this.close_sidebar_view_menu(cx);
            }
            _ => {}
        });
        self.sidebar.time_input = Some((input.clone(), events));
        input
    }

    /// A press or drag at window x on the slider: snap to the nearest stop
    /// and mirror it into the field.
    fn sample_sidebar_time_slider(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(track) = self.sidebar.time_track.get() else {
            return;
        };
        let width = f32::from(track.size.width);
        if width <= 0.0 {
            return;
        }
        let mins = sidebar_time::from_slider(f32::from(x - track.left()) / width);
        if mins == self.settings.sidebar_active_within_mins {
            return;
        }
        self.set_sidebar_active_within(mins, cx);
        if let Some((input, _)) = &self.sidebar.time_input {
            input.update(cx, |input, cx| input.set_text(time_field_text(mins), cx));
        }
    }

    /// The slider thumb's press state, 0 (resting capsule) → 1 (glass
    /// lens), on a spring — it overshoots a little either way.
    fn sidebar_time_press(&self) -> f32 {
        let target = if self.sidebar.time_pressed { 1.0 } else { 0.0 };
        let Some((from, at)) = self.sidebar.time_press else {
            return target;
        };
        if self.motion.reduced_motion {
            return target;
        }
        let (value, settled) = motion::spring(from, target, at.elapsed().as_secs_f32());
        if !settled {
            self.motion.active.set(true);
        }
        value
    }

    fn set_sidebar_time_pressed(&mut self, pressed: bool, cx: &mut Context<Self>) {
        if self.sidebar.time_pressed == pressed {
            return;
        }
        let from = self.sidebar_time_press();
        self.sidebar.time_pressed = pressed;
        self.sidebar.time_press = Some((from, std::time::Instant::now()));
        cx.notify();
    }

    /// The last-active slider, after the macOS 26 (Liquid Glass) slider: a
    /// rounded rail filled up to the thumb, one dot per stop beneath it, and
    /// scale labels under those. At rest the thumb is a white capsule; held,
    /// it springs into a clear glass lens that magnifies the rail and dots
    /// beneath it. Click jumps, drag scrubs (tracking past either end).
    fn render_sidebar_time_slider(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        /// The track box: tall enough for the swollen lens.
        const TRACK_H: f32 = 24.0;
        const RAIL: f32 = 5.0;
        /// Dot centers sit this far below the rail's center line.
        const DOT_DROP: f32 = 8.0;
        const DOT: f32 = 2.0;
        /// Thumb size at rest and held.
        const REST: (f32, f32) = (22.0, 14.0);
        const HELD: (f32, f32) = (36.0, 24.0);
        /// The lens's magnification of what it covers.
        const ZOOM: f32 = 1.45;
        /// Backdrop blur under the held lens (frosts the rail it covers).
        const BLUR: f32 = 2.5;
        let mid = TRACK_H / 2.0;
        let position = sidebar_time::slider_position(self.settings.sidebar_active_within_mins);
        let press = self.sidebar_time_press();
        let glass = press.clamp(0.0, 1.0);
        let measure = self.sidebar.time_track.clone();
        let track_w = self
            .sidebar
            .time_track
            .get()
            .map_or(0.0, |b| f32::from(b.size.width));
        let fill = theme.text_faint;
        let rail = theme.hairline(0.10);
        let dot = theme.hairline(0.28);
        let positions = sidebar_time::STOPS.len() + 1;
        let stop_at = |ix: usize| ix as f32 / (positions - 1) as f32;

        let dots = (0..positions).map(|ix| {
            div()
                .absolute()
                .top(px(mid + DOT_DROP - DOT / 2.0))
                .left(relative(stop_at(ix)))
                .ml(px(-DOT / 2.0))
                .size(px(DOT))
                .rounded_full()
                .bg(dot)
        });

        // The thumb: capsule → lens. Sizes ride the spring (and its
        // overshoot); colours and the lens contents blend on the clamped
        // value.
        let w = motion::lerp(REST.0, HELD.0, press);
        let h = motion::lerp(REST.1, HELD.1, press);
        let dark = theme.appearance.is_dark();
        let capsule = if dark {
            gpui::hsla(0.0, 0.0, 0.92, 1.0)
        } else {
            gpui::white()
        };
        // The lens is mostly clear: a thin veil of the menu surface hides the
        // unmagnified rail beneath, so only the magnified copy reads.
        let veil = theme
            .surface_overlay
            .opacity(if dark { 0.55 } else { 0.62 });
        let body = motion::mix(capsule, veil, glass);
        let shadow = |alpha: f32, y: f32, blur: f32| gpui::BoxShadow {
            color: gpui::black().opacity(alpha),
            offset: gpui::point(px(0.0), px(y)),
            blur_radius: px(blur),
            spread_radius: px(0.0),
            inset: false,
        };
        let inner = |color: Hsla, x: f32, y: f32, blur: f32| gpui::BoxShadow {
            color,
            offset: gpui::point(px(x), px(y)),
            blur_radius: px(blur),
            spread_radius: px(0.0),
            inset: true,
        };
        let shadows = vec![
            // Rest: a crisp contact shadow under the capsule.
            shadow(0.22 * (1.0 - glass), 0.5, 1.5),
            shadow(0.10 * (1.0 - glass), 1.0, 4.0),
            // Held: the lens floats — a wider, softer drop.
            shadow(0.16 * glass, 3.0, 10.0),
        ];

        // Inside the lens: the rail and dots around the thumb's centre,
        // magnified about it. Track point (x, y) lands at lens
        // ((x - centre) * zoom + w/2, (y - mid) * zoom + h/2).
        let zoom = motion::lerp(1.0, ZOOM, glass);
        let centre = position * track_w;
        let to_lens = |x: f32| (x - centre) * zoom + w / 2.0;
        let band = |from: f32, to: f32, color: Hsla| {
            div()
                .absolute()
                .left(px(to_lens(from)))
                .w(px((to - from) * zoom))
                .top(px(h / 2.0 - RAIL * zoom / 2.0))
                .h(px(RAIL * zoom))
                .rounded_full()
                .bg(color)
        };
        let radius = h / 2.0;
        let dot_y = DOT_DROP * zoom;
        // A dot only shows where the capsule outline covers it (the clip is
        // rectangular; the lens is not).
        let inset_at_dot = radius - (radius * radius - dot_y * dot_y).max(0.0).sqrt();
        let lens_dots = (0..positions).filter_map(|ix| {
            let x = to_lens(stop_at(ix) * track_w);
            let size = DOT * zoom;
            (x - size / 2.0 >= inset_at_dot && x + size / 2.0 <= w - inset_at_dot).then(|| {
                div()
                    .absolute()
                    .left(px(x - size / 2.0))
                    .top(px(h / 2.0 + dot_y - size / 2.0))
                    .size(px(size))
                    .rounded_full()
                    .bg(dot)
            })
        });
        let lens = div()
            .absolute()
            .inset_0()
            .opacity(glass)
            .child(band(0.0, track_w, rail))
            .child(band(0.0, centre, fill))
            .children(lens_dots);
        // Over the blur: the rim and the sheen stay crisp.
        let rim = div()
            .absolute()
            .inset_0()
            .rounded_full()
            .border_1()
            .border_color(motion::mix(
                theme.hairline(0.12),
                gpui::white().opacity(if dark { 0.35 } else { 0.85 }),
                glass,
            ))
            .shadow(vec![
                // Light caught on the upper edge, a refraction shade on the
                // lower one, and a soft inner glow.
                inner(gpui::white().opacity(0.95 * glass), 0.0, 1.0, 1.5),
                inner(gpui::black().opacity(0.10 * glass), 0.0, -1.0, 2.0),
                inner(gpui::white().opacity(0.55 * glass), 0.0, 0.0, 4.0),
            ])
            .when(glass > 0.0, |el| {
                el.child(div().absolute().inset_0().rounded_full().opacity(glass).bg(
                    gpui::linear_gradient(
                        180.0,
                        gpui::linear_color_stop(gpui::white().opacity(0.35), 0.0),
                        gpui::linear_color_stop(gpui::white().opacity(0.0), 0.55),
                    ),
                ))
            });
        // Paint order: body, magnified rail, then the blur over both (and
        // the real rail beneath), then the rim on top.
        let thumb = div()
            .absolute()
            .top(px(mid - h / 2.0))
            .left(relative(position))
            .ml(px(-w / 2.0))
            .w(px(w))
            .h(px(h))
            .rounded_full()
            .overflow_hidden()
            .shadow(shadows)
            .child(div().absolute().inset_0().rounded_full().bg(body))
            .when(glass > 0.0, |el| el.child(lens))
            .child(crate::kit::frost::frosted_always(
                h / 2.0,
                BLUR * glass,
                rim,
            ));

        let track = div()
            .relative()
            .w_full()
            .h(px(TRACK_H))
            .child(
                gpui::canvas(
                    move |bounds, _, _| measure.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(mid - RAIL / 2.0))
                    .h(px(RAIL))
                    .rounded_full()
                    .bg(rail),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(mid - RAIL / 2.0))
                    .w(relative(position))
                    .h(px(RAIL))
                    .rounded_full()
                    .bg(fill),
            )
            .children(dots)
            .child(thumb);
        let label = |text: &'static str, at: f32| {
            div()
                .absolute()
                .top_0()
                .left(relative(at))
                .ml(px(-14.0))
                .w(px(28.0))
                .flex()
                .justify_center()
                .child(SharedString::from(text))
        };
        let scale = |stop: usize| sidebar_time::slider_position(Some(sidebar_time::STOPS[stop]));
        div()
            .id("sidebar-view-time-slider")
            .flex_1()
            .min_w_0()
            .px(px(REST.0 / 2.0))
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.set_sidebar_time_pressed(true, cx);
                    this.sample_sidebar_time_slider(event.position.x, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.set_sidebar_time_pressed(false, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.set_sidebar_time_pressed(false, cx)),
            )
            .on_drag(SidebarTimeDrag, |_, _, _, cx| cx.new(|_| DragGhost))
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<SidebarTimeDrag>, _, cx| {
                    this.sample_sidebar_time_slider(event.event.position.x, cx);
                },
            ))
            // A release the up listeners missed (the drag machinery can eat
            // it): the next move with the button up lets go.
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.sidebar.time_pressed && event.pressed_button != Some(MouseButton::Left) {
                    this.set_sidebar_time_pressed(false, cx);
                }
            }))
            .child(track)
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(12.0))
                    .text_size(px(10.0))
                    .text_color(theme.text_faint)
                    .child(label("1h", scale(0)))
                    .child(label("12h", scale(4)))
                    .child(label("7d", scale(8)))
                    .child(label("All", 1.0)),
            )
            .into_any_element()
    }

    /// The sidebar view menu: a Devices section (All devices + one row per
    /// device, this device first), a Last active section (slider + typed
    /// window) and Sort / Order sections, each with a check on the current
    /// choice.
    pub(super) fn render_sidebar_view_menu(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(&position) = self.menus.sidebar_view.get() else {
            // Gone (closed and reaped): its field's focus goes with it.
            self.sidebar.time_input = None;
            self.sidebar.time_pressed = false;
            self.sidebar.time_press = None;
            return None;
        };
        let closing = self.menus.sidebar_view.closing_since();
        let (filter, sort) = (
            self.settings.sidebar_device_filter.clone(),
            self.settings.sidebar_sort,
        );
        let descending = self.sidebar_view().descending();
        let devices: Vec<(String, String)> = {
            let state = self.state.read(cx);
            let local = state.local_device_id.clone();
            let mut devices = state.devices.clone();
            devices.sort_by_key(|d| {
                (
                    local.as_deref() != Some(d.id.as_str()),
                    d.name.to_lowercase(),
                    d.id.clone(),
                )
            });
            devices.into_iter().map(|d| (d.id, d.name)).collect()
        };
        let check = |on: bool, theme: &Theme| {
            div().flex_none().size(px(14.0)).when(on, |el| {
                el.child(icon(icons::CHECK).size(px(13.0)).text_color(theme.text))
            })
        };
        let mut menu = popover::popover_card(theme)
            .w(px(236.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_sidebar_view_menu(cx);
            }))
            .flex()
            .flex_col()
            .child(popover::menu_heading(theme, "Devices"))
            .child(
                popover::menu_row(theme, false, "sidebar-view-all-devices")
                    .id("sidebar-view-all-devices")
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_sidebar_device_filter(None, cx)),
                    )
                    .child(check(filter.is_none(), theme))
                    .child(SharedString::from("All devices")),
            );
        for (id, name) in devices {
            let on = filter.as_deref() == Some(id.as_str());
            let pick = id.clone();
            menu = menu.child(
                popover::menu_row(theme, false, format!("sidebar-view-device-{id}"))
                    .id(SharedString::from(format!("sidebar-view-device-{id}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_sidebar_device_filter(Some(pick.clone()), cx)
                    }))
                    .child(check(on, theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name)),
                    ),
            );
        }
        menu = menu
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Last active"));
        let input = self.sidebar_time_input(cx);
        let readable = sidebar_time::parse(input.read(cx).text()).is_some();
        let slider = self.render_sidebar_time_slider(theme, cx);
        menu = menu.child(
            div()
                .px(px(8.0))
                .pb(px(6.0))
                .flex()
                .flex_row()
                .items_start()
                .gap(px(10.0))
                .child(slider)
                .child(
                    div()
                        .id("sidebar-view-time-field")
                        .flex_none()
                        .w(px(64.0))
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(if readable {
                            theme.hairline(0.10)
                        } else {
                            theme.danger
                        })
                        .bg(theme.ink(0.04))
                        .text_size(px(12.0))
                        .cursor_text()
                        .child(input),
                ),
        );
        menu = menu
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Sort by"));
        for option in crate::prefs::SidebarSort::ALL {
            menu = menu.child(
                popover::menu_row(
                    theme,
                    false,
                    format!("sidebar-view-sort-{}", option.label()),
                )
                .id(SharedString::from(format!(
                    "sidebar-view-sort-{}",
                    option.label()
                )))
                .on_click(cx.listener(move |this, _, _, cx| this.set_sidebar_sort(option, cx)))
                .child(check(sort == option, theme))
                .child(SharedString::from(option.label())),
            );
        }
        menu = menu
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Order"));
        for (label, glyph, desc) in [
            ("Ascending", icons::ARROW_UP, false),
            ("Descending", icons::ARROW_DOWN, true),
        ] {
            menu = menu.child(
                popover::menu_row(theme, false, format!("sidebar-view-order-{label}"))
                    .id(SharedString::from(format!("sidebar-view-order-{label}")))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_sidebar_descending(desc, cx)),
                    )
                    .child(check(descending == desc, theme))
                    .child(icon(glyph).size(px(13.0)).text_color(theme.text_muted))
                    .child(SharedString::from(label)),
            );
        }
        Some(popover::menu_at(
            "sidebar-view-menu-popover",
            position,
            menu.into_any_element(),
            closing,
        ))
    }

    /// Pin/unpin a session (synced): pinned sessions lead their project.
    pub(super) fn set_chat_pinned(
        &mut self,
        chat_id: String,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.close_chat_menu(cx);
        self.mutate(
            serde_json::json!({ "op": "setChatPinned", "chatId": chat_id, "pinned": pinned }),
            cx,
        );
        cx.notify();
    }

    pub(super) fn delete_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.dialogs.delete_chat = None;
        let orphan = {
            let state = self.state.read(cx);
            spaces::orphan_worktree_after_delete(&state.chats, &state.spaces, &chat_id)
        };
        // A quick chat's scratch folder dies with the row: captured before
        // the mutate while the row is still in the local list.
        let scratch = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|c| c.id == chat_id && c.is_scratch())
            .and_then(|c| c.cwd.clone().map(|cwd| (c.device_id.clone(), cwd)));
        // Its tab closes (the slot's composer, and its per-chat stage, go
        // with it); a stashed draft of a closed tab is dropped too.
        if let Some(sid) = self.slot_for_tab(&crate::workspace::TabKey::session(chat_id.clone()))
            && let Some(slot) = self.tiles.slots.get(&sid)
        {
            slot.composer
                .update(cx, |composer, _| composer.purge_chat(&chat_id));
        }
        self.close_tab(&crate::workspace::TabKey::session(chat_id.clone()), cx);
        self.closed_tabs.drafts.remove(&chat_id);
        self.mutate(
            serde_json::json!({ "op": "deleteChat", "chatId": chat_id.clone() }),
            cx,
        );
        if let Some((device_id, cwd)) = scratch {
            self.delete_scratch_dir(chat_id, device_id, cwd, cx);
        }
        // Last session of a linked worktree: ask whether to remove the
        // checkout too. Captured before the mutate so the row is still in
        // the local list; children of this chat cascade and don't count.
        self.dialogs.delete_worktree = orphan;
        cx.notify();
    }

    /// Remove a deleted quick chat's scratch folder on its host. The host
    /// verifies the path is the folder it minted for this chat id before
    /// deleting anything; a failure is surfaced, never retried silently.
    fn delete_scratch_dir(
        &mut self,
        chat_id: String,
        device_id: String,
        cwd: String,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let mut params = serde_json::json!({ "chatId": chat_id, "path": cwd });
        if local.as_deref() != Some(device_id.as_str())
            && let Some(object) = params.as_object_mut()
        {
            object.insert("targetDeviceId".into(), device_id.into());
        }
        self.scratch_cleanup_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine
                .client()
                .call(methods::DELETE_SCRATCH_DIR, params)
                .await
            {
                this.update(cx, |shell, cx| {
                    shell.sidebar.notice =
                        Some(format!("Scratch folder not removed: {err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    pub(super) fn delete_worktree(&mut self, orphan: OrphanWorktree, cx: &mut Context<Self>) {
        self.dialogs.delete_worktree = None;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sidebar.notice = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let local = self.state.read(cx).local_device_id.clone();
        let mut params = serde_json::Map::new();
        params.insert("repoPath".into(), orphan.repo_path.into());
        params.insert("worktreePath".into(), orphan.worktree_path.into());
        if local.as_deref() != Some(orphan.device_id.as_str()) {
            params.insert("targetDeviceId".into(), orphan.device_id.into());
        }
        self.delete_worktree_task = Some(cx.spawn(async move |this, cx| {
            if let Err(err) = engine
                .client()
                .call(methods::DELETE_WORKTREE, serde_json::Value::Object(params))
                .await
            {
                this.update(cx, |shell, cx| {
                    shell.sidebar.notice = Some(format!("{err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    pub(super) fn request_sign_out(&mut self, cx: &mut Context<Self>) {
        self.close_user_menu(cx);
        if self.state.read(cx).workspace_scope != Some(WorkspaceScope::Synced) {
            return;
        }
        self.sync.flow = SyncFlow::SignOutConfirm;
        cx.notify();
    }

    pub(super) fn confirm_sign_out(&mut self, cx: &mut Context<Self>) {
        self.start_local_runtime_transition(true, cx);
    }

    pub(super) fn start_local_runtime_transition(
        &mut self,
        sign_out: bool,
        cx: &mut Context<Self>,
    ) {
        if self.sync.runtime_change_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sync.runtime_change_error = Some("Engine not connected".into());
            self.sync.flow = SyncFlow::SignedOutRestartRequired;
            cx.notify();
            return;
        };
        self.sync.flow = SyncFlow::SigningOut;
        self.sync.runtime_change_error = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let shutdown_dir = data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            if sign_out {
                engine
                    .client()
                    .call(methods::SIGN_OUT, serde_json::json!({}))
                    .await
                    .map_err(|error| format!("Sign out failed: {error}"))?;
            }
            stop_synced_runtime(engine, ipc_socket, &shutdown_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.sync.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.sync.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        shell.sync.flow = SyncFlow::Idle;
                        shell.sync.runtime_change_error = None;
                        shell.sync.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), shell.data_dir.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync.flow = SyncFlow::SignedOutRestartRequired;
                        shell.sync.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn cancel_auth_setup(&mut self, cx: &mut Context<Self>) {
        let local = self.state.read(cx).workspace_scope == Some(WorkspaceScope::Local);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let pending_auth = self.sync.auth_task.take();
        let pending_org = self.sync.org.as_mut().and_then(|org| org.task.take());
        if local {
            self.sync.flow = SyncFlow::Canceling;
        }
        self.sync.auth_task = Some(cx.spawn(async move |this, cx| {
            // Do not race SignOut against an exchange or organization write
            // that can still persist a session after credentials were cleared.
            if let Some(task) = pending_auth {
                task.await;
            }
            if let Some(task) = pending_org {
                task.await;
            }
            let result = engine
                .client()
                .call(methods::SIGN_OUT, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| {
                match result {
                    Ok(_) => {
                        shell.sync.org = None;
                        if local {
                            shell.sync.flow = SyncFlow::Idle;
                        }
                    }
                    Err(err) => {
                        if local {
                            shell.sync.flow = SyncFlow::Enabling;
                        }
                        shell.sidebar.notice =
                            Some(format!("Could not cancel sign-in: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn postpone_sync_restart(&mut self, cx: &mut Context<Self>) {
        match self.sync.flow {
            SyncFlow::RestartPending { .. } => {
                self.sync.flow = SyncFlow::RestartPending { notice_open: false };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync.flow = SyncFlow::SwitchOffer { notice_open: false };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync.flow = SyncFlow::ImportFailed { notice_open: false };
            }
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn reopen_sync_notice(&mut self, cx: &mut Context<Self>) {
        self.close_user_menu(cx);
        match self.sync.flow {
            SyncFlow::RestartPending { .. } => {
                self.sync.flow = SyncFlow::RestartPending { notice_open: true };
            }
            SyncFlow::SwitchOffer { .. } => {
                self.sync.flow = SyncFlow::SwitchOffer { notice_open: true };
            }
            SyncFlow::ImportFailed { .. } => {
                self.sync.flow = SyncFlow::ImportFailed { notice_open: true };
            }
            _ => return,
        }
        cx.notify();
    }

    /// The wizard's choice step chose a path: stop the local runtime, boot the
    /// synced one in-place (mirror of the sign-out transition), then let
    /// [`Self::drive_sync_switch`] run the import once the runtime is ready.
    /// Failure falls back to the quit-and-reopen dialog — the local profile is
    /// untouched, so the old path is always a safe exit.
    pub(super) fn start_synced_switch(&mut self, import: bool, cx: &mut Context<Self>) {
        if self.sync.runtime_change_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sync.runtime_change_error = Some("Engine not connected".into());
            self.sync.flow = SyncFlow::RestartPending { notice_open: true };
            cx.notify();
            return;
        };
        self.sync.flow = SyncFlow::Switching { import };
        self.sync.runtime_change_error = None;
        self.sync.import_current = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let transition = Tokio::spawn(cx, async move {
            stop_synced_runtime(engine, ipc_socket, &data_dir).await
        });
        let state = self.state.clone();
        let boot = self.boot.clone();
        self.sync.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match transition.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.sync.runtime_change_task = None;
                match result {
                    Ok(()) => {
                        // Keep `Switching { import }`: the state observer sees
                        // the replacement runtime reach Ready and advances the
                        // wizard from there.
                        shell.sync.org = None;
                        shell.route = Route::Chat;
                        shell.space_boot_applied = false;
                        state.update(cx, |state, cx| state.prepare_runtime_replacement(cx));
                        AppState::bootstrap(state.clone(), shell.data_dir.clone(), boot, cx);
                    }
                    Err(error) => {
                        shell.sync.flow = SyncFlow::RestartPending { notice_open: true };
                        shell.sync.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Advance the in-place switch when the replacement runtime lands: Ready +
    /// Synced starts the import stream (or finishes immediately when the user
    /// chose a fresh start); a runtime that comes back non-synced fell out of
    /// the swap — surface the quit fallback rather than pretend.
    pub(super) fn drive_sync_switch(&mut self, cx: &mut Context<Self>) {
        let SyncFlow::Switching { import } = self.sync.flow else {
            return;
        };
        if self.sync.runtime_change_task.is_some() {
            return; // still stopping the local runtime
        }
        let (ready, scope) = {
            let state = self.state.read(cx);
            (
                matches!(state.connection, ConnectionStatus::Ready),
                state.workspace_scope,
            )
        };
        if !ready {
            if let ConnectionStatus::Failed(error) = &self.state.read(cx).connection {
                self.sync.flow = SyncFlow::RestartPending { notice_open: true };
                self.sync.runtime_change_error = Some(error.clone().into());
                cx.notify();
            }
            return;
        }
        match scope {
            Some(WorkspaceScope::Synced) => {
                if import {
                    self.spawn_local_import(cx);
                } else {
                    self.sync.flow = SyncFlow::Idle;
                    cx.notify();
                }
            }
            Some(_) => {
                self.sync.flow = SyncFlow::RestartPending { notice_open: true };
                self.sync.runtime_change_error =
                    Some("The synced workspace did not come up — restart to finish.".into());
                cx.notify();
            }
            None => {}
        }
    }

    /// Subscribe to the engine's one-time import stream and mirror its
    /// progress into the wizard.
    pub(super) fn spawn_local_import(&mut self, cx: &mut Context<Self>) {
        if self.sync.import_task.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sync.flow = SyncFlow::RestartPending { notice_open: true };
            self.sync.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        self.sync.flow = SyncFlow::Importing { done: 0, total: 0 };
        self.sync.runtime_change_error = None;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
        let stream = Tokio::spawn(cx, async move {
            let mut items = engine
                .client()
                .subscribe(methods::IMPORT_LOCAL_WORKSPACE, serde_json::json!({}))
                .await
                .map_err(|error| error.to_string())?;
            while let Some(item) = items.recv().await {
                let _ = tx.send(item);
            }
            Ok::<(), String>(())
        });
        self.sync.import_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let item = rx.recv().await;
                let ended = item.is_none();
                this.update(cx, |shell, cx| {
                    if let Some(item) = &item {
                        shell.apply_import_event(item, cx);
                    }
                    if ended {
                        shell.sync.import_task = None;
                        shell.sync.import_current = None;
                        // A stream that died before its summary is a failure —
                        // offer the in-place retry (idempotent).
                        if matches!(shell.sync.flow, SyncFlow::Importing { .. }) {
                            shell.sync.flow = SyncFlow::ImportFailed { notice_open: true };
                            shell.sync.runtime_change_error =
                                Some("The import stream ended before it finished.".into());
                        }
                        cx.notify();
                    }
                })
                .ok();
                if ended {
                    break;
                }
            }
            if let Ok(Err(error)) = stream.await {
                this.update(cx, |shell, cx| {
                    shell.sync.import_task = None;
                    if matches!(shell.sync.flow, SyncFlow::Importing { .. }) {
                        shell.sync.flow = SyncFlow::ImportFailed { notice_open: true };
                        shell.sync.runtime_change_error = Some(error.into());
                        cx.notify();
                    }
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn apply_import_event(&mut self, item: &serde_json::Value, cx: &mut Context<Self>) {
        match item.get("kind").and_then(|k| k.as_str()) {
            Some("start") => {
                let total = item.get("chats").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.sync.flow = SyncFlow::Importing { done: 0, total };
            }
            Some("chat") => {
                let index = item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let total = item.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.sync.import_current = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|t| SharedString::from(t.to_string()));
                self.sync.flow = SyncFlow::Importing { done: index, total };
            }
            Some("summary") => {
                self.sync.import_current = None;
                // A summary with errors is a FAILED import, however normally
                // the stream ended — never present a partial migration as
                // complete (the engine keeps collecting per-item failures
                // precisely so this can be surfaced).
                match import_summary_outcome(item) {
                    Ok((imported, skipped)) => {
                        self.sync.flow = SyncFlow::ImportDone { imported, skipped };
                    }
                    Err(message) => {
                        self.sync.flow = SyncFlow::ImportFailed { notice_open: true };
                        self.sync.runtime_change_error = Some(message.into());
                    }
                }
            }
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn quit_for_runtime_change(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.sync.runtime_change_error = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        if engine.mode() == EngineMode::InProcess {
            cx.quit();
            return;
        }
        if self.sync.runtime_change_task.is_some() {
            return;
        }

        self.sync.runtime_change_error = None;
        let ipc_socket = self.boot.ipc_socket.clone();
        let data_dir = self.boot.data_dir.clone();
        let shutdown = Tokio::spawn(cx, async move {
            engine
                .client()
                .call(methods::STOP_ENGINE, serde_json::json!({}))
                .await
                .map_err(|err| err.to_string())?;
            wait_for_remote_engine_shutdown(ipc_socket, &data_dir, RUNTIME_CHANGE_TIMEOUT).await
        });
        self.sync.runtime_change_task = Some(cx.spawn(async move |this, cx| {
            let result = match shutdown.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.sync.runtime_change_task = None;
                match result {
                    Ok(_) => cx.quit(),
                    Err(err) => {
                        shell.sync.runtime_change_error = Some(format!(
                            "Could not stop the remote engine: {err}. Run `cypher daemon stop`, then quit and reopen Cypher."
                        ).into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn start_sign_in(&mut self, cx: &mut Context<Self>) {
        let scope = self.state.read(cx).workspace_scope;
        if scope == Some(WorkspaceScope::Development) {
            return;
        }
        self.close_user_menu(cx);
        if scope == Some(WorkspaceScope::Local) {
            self.sync.flow = SyncFlow::Enabling;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.sync.auth_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::SIGN_IN, serde_json::json!({}))
                .await;
            this.update(cx, |shell, cx| match result {
                Ok(value) => {
                    if let Some(url) = value.get("url").and_then(|u| u.as_str()) {
                        cx.open_url(url);
                    }
                    cx.notify();
                }
                Err(err) => {
                    if scope == Some(WorkspaceScope::Local) && shell.sync.flow == SyncFlow::Enabling
                    {
                        shell.sync.flow = SyncFlow::Idle;
                    }
                    shell.sidebar.notice = Some(format!("Sign in failed: {err}").into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}
