//! Small RGB-only picker built on the existing GPUI/popover/input stack.
//! Dragging changes a local draft; release writes through the existing HEX
//! input, so validation, persistence errors and region scoping stay centralized.
use crate::{
    chat_style,
    composer::{ComposerInput, ComposerInputEvent},
    icons, popover,
    theme::Theme,
};
use gpui::{
    AppContext, Bounds, Context, Entity, FocusHandle, Hsla, MouseButton, Pixels, Point, Render,
    Subscription, Window, div, prelude::*, px, relative,
};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug)]
struct Hsv {
    h: f32,
    s: f32,
    v: f32,
}
fn unit(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
impl Hsv {
    fn from_color(color: Hsla, previous_hue: f32) -> Self {
        let c = gpui::Rgba::from(color);
        let max = c.r.max(c.g).max(c.b);
        let min = c.r.min(c.g).min(c.b);
        let d = max - min;
        let h = if d <= f32::EPSILON {
            previous_hue
        } else if max == c.r {
            ((c.g - c.b) / d).rem_euclid(6.0) / 6.0
        } else if max == c.g {
            ((c.b - c.r) / d + 2.0) / 6.0
        } else {
            ((c.r - c.g) / d + 4.0) / 6.0
        };
        Self {
            h: unit(h),
            s: if max <= 0.0 { 0.0 } else { d / max },
            v: unit(max),
        }
    }
    fn color(self) -> Hsla {
        let h = unit(self.h).rem_euclid(1.0) * 6.0;
        let c = unit(self.v) * unit(self.s);
        let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match h as u8 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = unit(self.v) - c;
        gpui::Rgba {
            r: r + m,
            g: g + m,
            b: b + m,
            a: 1.0,
        }
        .into()
    }
}
fn hex(color: Hsla) -> String {
    let c = gpui::Rgba::from(color);
    format!(
        "#{:02X}{:02X}{:02X}",
        (unit(c.r) * 255.0).round() as u8,
        (unit(c.g) * 255.0).round() as u8,
        (unit(c.b) * 255.0).round() as u8
    )
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Track {
    SaturationValue,
    Hue,
}
type Measured = Rc<Cell<Option<Bounds<Pixels>>>>;
const PALETTE: [u32; 16] = [
    0x000000, 0xffffff, 0x334155, 0x94a3b8, 0xef4444, 0xf97316, 0xf59e0b, 0xfacc15, 0x22c55e,
    0x10b981, 0x14b8a6, 0x06b6d4, 0x3b82f6, 0x6366f1, 0xa855f7, 0xec4899,
];

pub(super) struct ColorPicker {
    output: Entity<ComposerInput>,
    input: Entity<ComposerInput>,
    value: Hsla,
    hsv: Hsv,
    draft_hex: String,
    invalid: bool,
    open: bool,
    dark_palette: Option<bool>,
    drag: Option<Track>,
    trigger_bounds: Measured,
    sv_bounds: Measured,
    hue_bounds: Measured,
    trigger_focus: FocusHandle,
    popup_focus: FocusHandle,
    _subscription: Subscription,
}
impl ColorPicker {
    pub fn new(output: Entity<ComposerInput>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| ComposerInput::settings_field("#RRGGBB", false, cx));
        let subscription = cx.subscribe(&input, |this: &mut Self, input, event, cx| {
            if !this.open
                || !matches!(
                    event,
                    ComposerInputEvent::Edited | ComposerInputEvent::Submitted
                )
            {
                return;
            }
            let raw = input.read(cx).text().to_string();
            // Programmatic updates must not turn an inherited value into an
            // explicit override merely because the picker was opened.
            if raw == this.draft_hex {
                return;
            }
            this.draft_hex = raw.clone();
            if let Some(value) = chat_style::color(&Some(raw)) {
                this.invalid = false;
                this.hsv = Hsv::from_color(value, this.hsv.h);
                this.commit(cx);
            } else {
                this.invalid = true;
            }
            cx.notify();
        });
        let value = gpui::rgb(0xffffff).into();
        Self {
            output,
            input,
            value,
            hsv: Hsv::from_color(value, 0.0),
            draft_hex: String::new(),
            invalid: false,
            open: false,
            dark_palette: None,
            drag: None,
            trigger_bounds: Rc::new(Cell::new(None)),
            sv_bounds: Rc::new(Cell::new(None)),
            hue_bounds: Rc::new(Cell::new(None)),
            trigger_focus: cx.focus_handle(),
            popup_focus: cx.focus_handle(),
            _subscription: subscription,
        }
    }
    pub fn sync(&mut self, value: Hsla, dark_palette: bool, cx: &mut Context<Self>) {
        let changed_palette = self.dark_palette != Some(dark_palette);
        self.dark_palette = Some(dark_palette);
        if changed_palette {
            self.close(cx);
        }
        if changed_palette || hex(value) != hex(self.value) {
            self.hsv = Hsv::from_color(value, self.hsv.h);
            self.sync_hex(cx);
            cx.notify();
        }
        self.value = value;
    }
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.drag = None;
            self.hsv = Hsv::from_color(self.value, self.hsv.h);
            cx.notify();
        }
    }
    fn sync_hex(&mut self, cx: &mut Context<Self>) {
        self.draft_hex = hex(self.hsv.color());
        self.invalid = false;
        let text = self.draft_hex.clone();
        if self.input.read(cx).text() != text {
            self.input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }
    fn commit(&mut self, cx: &mut Context<Self>) {
        let value = self.hsv.color();
        let text = hex(value);
        self.value = value;
        self.sync_hex(cx);
        // Always emit the user edit: choosing the same HEX again must also
        // retry a failed save. The owning editor deduplicates persisted values.
        self.output.update(cx, |input, cx| input.set_text(text, cx));
    }
    fn sample(&mut self, track: Track, position: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = match track {
            Track::Hue => self.hue_bounds.get(),
            Track::SaturationValue => self.sv_bounds.get(),
        };
        let Some(bounds) = bounds else {
            return;
        };
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let x = unit(f32::from(position.x - bounds.left()) / w);
        let y = unit(f32::from(position.y - bounds.top()) / h);
        match track {
            Track::Hue => self.hsv.h = x,
            Track::SaturationValue => {
                self.hsv.s = x;
                self.hsv.v = 1.0 - y;
            }
        }
        // Keep HEX in sync during a drag, without publishing each move.
        self.sync_hex(cx);
        cx.notify();
    }
    fn finish(&mut self, track: Track, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.drag == Some(track) {
            self.sample(track, position, cx);
            self.drag = None;
            self.commit(cx);
            cx.notify();
        }
    }
    fn track(&self, track: Track, cx: &mut Context<Self>) -> gpui::AnyElement {
        let measure = match track {
            Track::Hue => self.hue_bounds.clone(),
            Track::SaturationValue => self.sv_bounds.clone(),
        };
        let mut area = div()
            .id(if track == Track::Hue {
                "picker-hue"
            } else {
                "picker-sv"
            })
            .w_full()
            .h(px(if track == Track::Hue { 18.0 } else { 150.0 }))
            .relative()
            .cursor(gpui::CursorStyle::Crosshair)
            .child(
                gpui::canvas(
                    move |bounds, _, _| measure.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    window.prevent_default();
                    this.popup_focus.focus(window, cx);
                    this.drag = Some(track);
                    this.sample(track, event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    if this.drag == Some(track) && event.pressed_button == Some(MouseButton::Left) {
                        this.sample(track, event.position, cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseUpEvent, _, cx| {
                    this.finish(track, event.position, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseUpEvent, _, cx| {
                    this.finish(track, event.position, cx);
                }),
            );
        if track == Track::Hue {
            area = area
                .child(div().absolute().inset_0().flex().children((0..6).map(|i| {
                    div()
                        .flex_1()
                        .h_full()
                        .when(i == 0, |el| el.rounded_l(px(4.0)))
                        .when(i == 5, |el| el.rounded_r(px(4.0)))
                        .bg(gpui::linear_gradient(
                            90.0,
                            gpui::linear_color_stop(
                                Hsv {
                                    h: i as f32 / 6.0,
                                    s: 1.0,
                                    v: 1.0,
                                }
                                .color(),
                                0.0,
                            ),
                            gpui::linear_color_stop(
                                Hsv {
                                    h: (i + 1) as f32 / 6.0,
                                    s: 1.0,
                                    v: 1.0,
                                }
                                .color(),
                                1.0,
                            ),
                        ))
                })))
                .child(
                    div()
                        .absolute()
                        .top(px(-2.0))
                        .left(relative(self.hsv.h))
                        .ml(px(-3.0))
                        .w(px(6.0))
                        .h(px(22.0))
                        .rounded(px(3.0))
                        .border_2()
                        .border_color(gpui::rgb(0xffffff)),
                );
        } else {
            area = area
                .rounded(px(4.0))
                .bg(Hsv {
                    h: self.hsv.h,
                    s: 1.0,
                    v: 1.0,
                }
                .color())
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(4.0))
                        .bg(gpui::linear_gradient(
                            90.0,
                            gpui::linear_color_stop(gpui::rgb(0xffffff), 0.0),
                            gpui::linear_color_stop(gpui::transparent_white(), 1.0),
                        )),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(4.0))
                        .bg(gpui::linear_gradient(
                            180.0,
                            gpui::linear_color_stop(gpui::transparent_black(), 0.0),
                            gpui::linear_color_stop(gpui::rgb(0x000000), 1.0),
                        )),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(self.hsv.s))
                        .top(relative(1.0 - self.hsv.v))
                        .ml(px(-5.0))
                        .mt(px(-5.0))
                        .size(px(10.0))
                        .rounded_full()
                        .border_2()
                        .border_color(gpui::rgb(0xffffff))
                        .shadow_sm(),
                );
        }
        area.into_any_element()
    }
}
impl Render for ColorPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let measure = self.trigger_bounds.clone();
        let popup = if self.open {
            let colors = div().flex().flex_wrap().gap(px(6.0)).children(
                PALETTE.into_iter().enumerate().map(|(i, color)| {
                    div()
                        .id(("picker-preset", i))
                        .size(px(22.0))
                        .rounded(px(4.0))
                        .bg(gpui::rgb(color))
                        .border_1()
                        .border_color(theme.border_strong)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hsv = Hsv::from_color(gpui::rgb(color).into(), this.hsv.h);
                            this.commit(cx);
                            cx.notify();
                        }))
                }),
            );
            let panel = popover::popover_card(&theme)
                .id("color-picker-panel")
                .w(px(264.0))
                .p(px(12.0))
                .track_focus(&self.popup_focus)
                .flex()
                .flex_col()
                .gap(px(12.0))
                .on_click(|_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close(cx);
                        this.trigger_focus.focus(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    if !this
                        .trigger_bounds
                        .get()
                        .is_some_and(|b| b.contains(&event.position))
                    {
                        this.close(cx);
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(super::widgets::field_label(&theme, "Color").flex_1())
                        .child(
                            div()
                                .id("picker-close")
                                .size(px(20.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .child(
                                    icons::icon(icons::CLOSE)
                                        .size(px(12.0))
                                        .text_color(theme.text_muted),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close(cx);
                                    this.trigger_focus.focus(window, cx);
                                })),
                        ),
                )
                .child(self.track(Track::SaturationValue, cx))
                .child(self.track(Track::Hue, cx))
                .child(colors)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().size(px(26.0)).rounded(px(4.0)).bg(self.hsv.color()))
                        .child(div().flex_1().min_w_0().child(self.input.clone())),
                )
                .when(self.invalid, |el| {
                    el.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.danger)
                            .child("Use a six-digit HEX color."),
                    )
                });
            Some(popover::anchored_menu_below(
                "appearance-color-picker",
                panel.into_any_element(),
                None,
            ))
        } else {
            None
        };
        div()
            .id("color-picker-trigger")
            .size(px(22.0))
            .flex_none()
            .relative()
            .track_focus(&self.trigger_focus)
            .rounded(px(5.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(self.value)
            .cursor_pointer()
            .child(
                gpui::canvas(
                    move |bounds, _, _| measure.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.open {
                    this.close(cx);
                    this.trigger_focus.focus(window, cx);
                } else {
                    this.open = true;
                    this.drag = None;
                    this.hsv = Hsv::from_color(this.value, this.hsv.h);
                    this.sync_hex(cx);
                    this.popup_focus.focus(window, cx);
                    cx.notify();
                }
            }))
            .children(popup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rgb_round_trip_and_alpha_ignored() {
        for r in [0, 32, 128, 223, 255] {
            for g in [0, 32, 128, 223, 255] {
                for b in [0, 32, 128, 223, 255] {
                    let c: Hsla = gpui::rgb((r << 16) | (g << 8) | b).into();
                    assert_eq!(hex(Hsv::from_color(c.opacity(0.25), 0.0).color()), hex(c));
                }
            }
        }
    }
    #[test]
    fn grey_preserves_selected_hue_and_edges_are_finite() {
        assert_eq!(Hsv::from_color(gpui::rgb(0x888888).into(), 0.7).h, 0.7);
        assert_eq!(
            hex(Hsv {
                h: 1.0,
                s: 1.0,
                v: 1.0
            }
            .color()),
            "#FF0000"
        );
        assert_eq!(
            hex(Hsv {
                h: 0.3,
                s: 0.0,
                v: 1.0
            }
            .color()),
            "#FFFFFF"
        );
        assert_eq!(
            hex(Hsv {
                h: 0.3,
                s: 1.0,
                v: 0.0
            }
            .color()),
            "#000000"
        );
        assert_eq!(unit(f32::NAN), 0.0);
        assert_eq!(unit(-1.0), 0.0);
        assert_eq!(unit(2.0), 1.0);
    }
}
