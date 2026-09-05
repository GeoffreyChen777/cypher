//! Overall theme selection and lazily expanded per-region color controls.
use crate::chat_style::{ColorPreset, normalize_hex};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::settings::widgets;
use crate::surface_style::{self, FIELDS, Field, Palette, Region, SurfaceAppearanceState};
use crate::theme::{Appearance, Theme};
use gpui::{
    AnyElement, Context, Entity, Render, SharedString, Subscription, Window, div, prelude::*, px,
};

pub(super) struct SurfaceStyleEditor {
    region: Option<Region>,
    palette: Appearance,
    follow: bool,
    expanded: bool,
    advanced: bool,
    fields: Vec<Field>,
    inputs: Vec<Entity<ComposerInput>>,
    color_pickers: Vec<Entity<super::color_picker::ColorPicker>>,
    invalid: Vec<bool>,
    last: Palette,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl SurfaceStyleEditor {
    pub fn new(region: Option<Region>, cx: &mut Context<Self>) -> Self {
        let palette = Theme::of(cx).appearance;
        let last = surface_style::settings(cx).palette(palette).clone();
        let fields: Vec<_> = FIELDS
            .iter()
            .copied()
            .filter(|f| Some(f.region) == region)
            .collect();
        let inputs: Vec<_> = fields
            .iter()
            .map(|field| {
                cx.new(|cx| {
                    let mut input =
                        ComposerInput::settings_field("Follow overall theme", false, cx);
                    input.set_text(
                        last.overrides.get(field.key).cloned().unwrap_or_default(),
                        cx,
                    );
                    input
                })
            })
            .collect();
        let color_pickers = inputs
            .iter()
            .map(|input| cx.new(|cx| super::color_picker::ColorPicker::new(input.clone(), cx)))
            .collect();
        let mut subscriptions = Vec::new();
        for (index, input) in inputs.iter().enumerate() {
            subscriptions.push(cx.subscribe(input, move |this: &mut Self, _, event, cx| {
                if matches!(
                    event,
                    ComposerInputEvent::Edited | ComposerInputEvent::Submitted
                ) {
                    this.edit(index, cx);
                }
            }));
        }
        subscriptions.push(
            cx.observe_global::<SurfaceAppearanceState>(|this: &mut Self, cx| {
                if surface_style::settings(cx).palette(this.palette) != &this.last {
                    this.sync(cx);
                }
                cx.notify();
            }),
        );
        subscriptions.push(cx.observe_global::<Theme>(|this: &mut Self, cx| {
            if this.follow && this.palette != Theme::of(cx).appearance {
                this.palette = Theme::of(cx).appearance;
                this.sync(cx);
            }
            cx.notify();
        }));
        Self {
            region,
            palette,
            follow: true,
            expanded: false,
            advanced: false,
            invalid: vec![false; fields.len()],
            fields,
            inputs,
            color_pickers,
            last,
            error: None,
            _subscriptions: subscriptions,
        }
    }
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.last = surface_style::settings(cx).palette(self.palette).clone();
        self.invalid.fill(false);
        for (field, input) in self.fields.iter().zip(&self.inputs) {
            let value = self
                .last
                .overrides
                .get(field.key)
                .cloned()
                .unwrap_or_default();
            if input.read(cx).text() != value {
                input.update(cx, |input, cx| input.set_text(value, cx));
            }
        }
    }
    fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Palette)) {
        let mut settings = surface_style::settings(cx).clone();
        edit(settings.palette_mut(self.palette));
        self.error = surface_style::set(settings, cx)
            .err()
            .map(|e| format!("Could not save colors: {e}").into());
        cx.notify();
    }
    fn edit(&mut self, index: usize, cx: &mut Context<Self>) {
        let raw = self.inputs[index].read(cx).text().trim().to_owned();
        let value = if raw.is_empty() {
            Ok(None)
        } else {
            normalize_hex(&raw).map(Some).ok_or(())
        };
        self.invalid[index] = value.is_err();
        if let Ok(value) = value {
            let key = self.fields[index].key;
            if surface_style::settings(cx)
                .palette(self.palette)
                .overrides
                .get(key)
                != value.as_ref()
            {
                self.change(cx, |p| {
                    if let Some(value) = value {
                        p.overrides.insert(key.into(), value);
                    } else {
                        p.overrides.remove(key);
                    }
                });
            }
        }
        cx.notify();
    }
    fn palette_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap(px(8.0))
            .children([Appearance::Light, Appearance::Dark].map(|palette| {
                widgets::ghost_action(theme)
                    .id(SharedString::from(format!(
                        "surface-palette-{:?}-{palette:?}",
                        self.region
                    )))
                    .bg(if palette == self.palette {
                        theme.element_active
                    } else {
                        gpui::transparent_black()
                    })
                    .child(if palette.is_dark() {
                        "Dark colors"
                    } else {
                        "Light colors"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.palette = palette;
                        this.follow = palette == Theme::of(cx).appearance;
                        this.sync(cx);
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }
    fn color_row(
        &self,
        index: usize,
        resolved: &Theme,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let field = self.fields[index];
        let inherited = !self.last.overrides.contains_key(field.key);
        let swatch = field.value(resolved);
        self.color_pickers[index].update(cx, |picker, cx| {
            picker.sync(swatch, self.palette.is_dark(), cx)
        });
        super::setting_row()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(widgets::row_title(theme, field.label))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(if inherited {
                                "Following overall theme"
                            } else {
                                "Custom override"
                            }),
                    )
                    .when(self.invalid[index], |el| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.danger_muted)
                                .child("Use #RRGGBB, or clear to inherit."),
                        )
                    }),
            )
            .child(self.color_pickers[index].clone())
            .child(
                div()
                    .w(px(172.0))
                    .min_w_0()
                    .child(self.inputs[index].clone()),
            )
            .child(
                widgets::ghost_action(theme)
                    .id(SharedString::from(format!("reset-{}", field.key)))
                    .child("Reset")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change(cx, |p| {
                            p.overrides.remove(field.key);
                        });
                        this.sync(cx);
                    })),
            )
            .into_any_element()
    }
}

impl Render for SurfaceStyleEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let palette = surface_style::settings(cx).palette(self.palette).clone();
        let base = Theme::for_appearance(self.palette);
        let title = self.region.map(Region::label).unwrap_or("Color theme");
        let mut body = super::section_frame(&theme, false);
        let reset = widgets::ghost_action(&theme)
            .id(SharedString::from(format!(
                "reset-surface-{:?}",
                self.region
            )))
            .child("Reset")
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                let region = this.region;
                this.change(cx, |p| {
                    if let Some(region) = region {
                        p.reset_region(region);
                    } else {
                        p.preset = ColorPreset::Default;
                    }
                });
                this.sync(cx);
            }))
            .into_any_element();
        let heading = super::section_header(
            &theme,
            title,
            self.expanded,
            Some(reset),
            cx.listener(|this, _, _, cx| {
                this.expanded = !this.expanded;
                if !this.expanded {
                    for picker in &this.color_pickers {
                        picker.update(cx, |picker, cx| picker.close(cx));
                    }
                }
                cx.notify();
            }),
        );
        body = body.child(heading);
        if !self.expanded {
            return body.into_any_element();
        }
        let mut card = super::content_group()
            .mt(px(super::SECTION_BODY_GAP))
            .child(div().child(self.palette_picker(&theme, cx)));
        if let Some(region) = self.region {
            let resolved = surface_style::resolve(&palette, &base, region);
            card = card.child(div().text_size(px(11.0)).text_color(theme.text_muted)
                .child(match region {
                    Region::Terminal => "Default text and ANSI 0–15 only; application true-color output is preserved. Cursor and selection use translucent tints.",
                    Region::Git => "Base text and line backgrounds only. Colored syntax highlights and + / − markers stay unchanged.",
                    Region::Sidebar => "Project cards and sidebar text only. The sidebar and window backing keep their fixed glass material in every preset.",
                }))
                .child(region_preview(region, &resolved));
            for (index, field) in self.fields.iter().enumerate() {
                if field.ansi_index().is_none() {
                    card = card.child(self.color_row(index, &resolved, &theme, cx));
                }
            }
            if region == Region::Terminal {
                card = card.child(div().mt(px(super::SECTION_BODY_GAP)).child(
                    super::section_header(
                        &theme,
                        "Advanced: ANSI 16 colors",
                        self.advanced,
                        None,
                        cx.listener(|this, _, _, cx| {
                            this.advanced = !this.advanced;
                            if !this.advanced {
                                for picker in &this.color_pickers {
                                    picker.update(cx, |picker, cx| picker.close(cx));
                                }
                            }
                            cx.notify();
                        }),
                    ),
                ));
                if self.advanced {
                    for (index, field) in self.fields.iter().enumerate() {
                        if field.ansi_index().is_some() {
                            card = card.child(self.color_row(index, &resolved, &theme, cx));
                        }
                    }
                }
            }
            let warnings = surface_style::contrast_warnings(region, &resolved);
            if !warnings.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.warning_muted)
                        .child(format!(
                            "Low contrast (below 4.5:1): {}. Colors are not automatically changed.",
                            warnings.join(", ")
                        )),
                );
            }
        } else {
            let mut buttons = div().flex().flex_wrap().gap(px(8.0));
            for preset in ColorPreset::ALL {
                buttons = buttons.child(
                    widgets::ghost_action(&theme)
                        .id(SharedString::from(format!("overall-{}", preset.label())))
                        .border_1()
                        .border_color(if palette.preset == preset {
                            theme.accent
                        } else {
                            theme.border
                        })
                        .child(preset.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Intentionally update ONLY the baseline selector.
                            this.change(cx, |p| p.preset = preset);
                        })),
                );
            }
            card = card.child(buttons);
        }
        if self.palette != theme.appearance {
            card = card.child(div().text_size(px(11.0)).text_color(theme.text_muted)
                .child("Editing the inactive palette. The preview changes now; the app uses these colors when that appearance is active."));
        }
        body.child(card)
            .when_some(self.error.clone(), |el, error| {
                el.child(widgets::error_strip(&theme, error))
            })
            .into_any_element()
    }
}

fn region_preview(region: Region, t: &Theme) -> AnyElement {
    match region {
        Region::Terminal => {
            use crate::terminal::{emulator::CellColor, view};
            let ansi = div()
                .flex()
                .flex_wrap()
                .gap(px(6.0))
                .children((0..16).map(|i| {
                    div()
                        .text_color(view::resolve_color(CellColor::Indexed(i), t))
                        .child(format!("{i:02}"))
                }));
            div()
                .p(px(12.0))
                .rounded(px(8.0))
                .bg(view::background(t))
                .font_family(t.font_mono.clone())
                .text_size(px(view::TERM_FONT_SIZE))
                .line_height(px(view::TERM_LINE_HEIGHT))
                .text_color(view::resolve_color(CellColor::Foreground, t))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child("Terminal preview · no commands are executed")
                .child(ansi)
                .child(
                    div()
                        .flex()
                        .child(div().bg(view::selection(t)).child("selected output"))
                        .child("  $ ")
                        .child(div().w(px(8.0)).h(px(16.0)).bg(t.cursor)),
                )
                .into_any_element()
        }
        Region::Git => crate::changes::color_preview(t),
        Region::Sidebar => {
            let hover = surface_style::sidebar_hover(t);
            div()
                .p(px(12.0))
                .rounded(px(8.0))
                .bg(t.glass())
                .text_size(px(13.0))
                .text_color(t.text)
                .child(
                    div()
                        .p(px(10.0))
                        .rounded(px(12.0))
                        .bg(t.surface)
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child("Example project")
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(t.text_muted)
                                .child("Current checkout · local device"),
                        )
                        .child(
                            div()
                                .px(px(8.0))
                                .py(px(6.0))
                                .rounded(px(8.0))
                                .bg(surface_style::sidebar_selected(t))
                                .child("Selected conversation"),
                        )
                        .child(
                            div()
                                .id("sidebar-preview-hover")
                                .px(px(8.0))
                                .py(px(6.0))
                                .rounded(px(8.0))
                                .hover(move |s| s.bg(hover))
                                .child("Hover to preview another row"),
                        ),
                )
                .into_any_element()
        }
    }
}
