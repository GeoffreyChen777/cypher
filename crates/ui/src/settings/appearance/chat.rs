//! Chat-only appearance controls. Native inputs stay on the normal UI font.
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, Window, div,
    prelude::*, px,
};

use crate::chat_style::{self, ChatAppearance, ChatAppearanceState, ChatColors};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::markdown::{parser, render};
use crate::settings::widgets;
use crate::theme::{Appearance, Theme};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FontKind {
    Body,
    Code,
}

#[derive(Clone, Copy)]
enum NumberSetting {
    BodySize,
    CodeSize,
    LineSpacing,
    ParagraphSpacing,
    MessageSpacing,
}
impl NumberSetting {
    const ALL: [Self; 5] = [
        Self::BodySize,
        Self::CodeSize,
        Self::LineSpacing,
        Self::ParagraphSpacing,
        Self::MessageSpacing,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::BodySize => "Message font size",
            Self::CodeSize => "Code font size",
            Self::LineSpacing => "Line spacing",
            Self::ParagraphSpacing => "Paragraph spacing",
            Self::MessageSpacing => "Message spacing",
        }
    }
    fn range(self) -> (f32, f32, f32) {
        match self {
            Self::BodySize => (12.0, 32.0, 1.0),
            Self::CodeSize => (10.0, 24.0, 0.5),
            Self::LineSpacing => (80.0, 200.0, 5.0),
            Self::ParagraphSpacing => (0.0, 40.0, 2.0),
            Self::MessageSpacing => (4.0, 64.0, 2.0),
        }
    }
    fn get(self, s: &ChatAppearance) -> f32 {
        match self {
            Self::BodySize => s.font_size,
            Self::CodeSize => s.code_font_size,
            Self::LineSpacing => s.line_spacing * 100.0,
            Self::ParagraphSpacing => s.paragraph_spacing,
            Self::MessageSpacing => s.message_spacing,
        }
    }
    fn set(self, s: &mut ChatAppearance, value: f32) {
        match self {
            Self::BodySize => s.font_size = value,
            Self::CodeSize => s.code_font_size = value,
            Self::LineSpacing => s.line_spacing = value / 100.0,
            Self::ParagraphSpacing => s.paragraph_spacing = value,
            Self::MessageSpacing => s.message_spacing = value,
        }
    }
    fn display(self, value: f32) -> String {
        if matches!(self, Self::LineSpacing) {
            return format!("{value:.0}%");
        }
        if value.fract().abs() < 0.01 {
            format!("{value:.0} px")
        } else {
            format!("{value:.1} px")
        }
    }
}

#[derive(Clone, Copy)]
enum ColorField {
    Text,
    Background,
    Accent,
    Bubble,
    CodeBackground,
    CodeText,
    InlineText,
    InlineBackground,
}
impl ColorField {
    const ALL: [Self; 8] = [
        Self::Text,
        Self::Background,
        Self::Accent,
        Self::Bubble,
        Self::CodeBackground,
        Self::CodeText,
        Self::InlineText,
        Self::InlineBackground,
    ];
    fn index(self) -> usize {
        self as usize
    }
    fn label(self) -> &'static str {
        match self {
            Self::Text => "Message text",
            Self::Background => "Chat background",
            Self::Accent => "Links & accent",
            Self::Bubble => "User message bubble",
            Self::CodeBackground => "Code block background",
            Self::CodeText => "Code block text",
            Self::InlineText => "Inline code text",
            Self::InlineBackground => "Inline code background",
        }
    }
    fn get(self, colors: &ChatColors) -> &Option<String> {
        match self {
            Self::Text => &colors.text,
            Self::Background => &colors.background,
            Self::Accent => &colors.accent,
            Self::Bubble => &colors.user_bubble,
            Self::CodeBackground => &colors.code_block_background,
            Self::CodeText => &colors.code_block_text,
            Self::InlineText => &colors.inline_code_text,
            Self::InlineBackground => &colors.inline_code_background,
        }
    }
    fn set(self, colors: &mut ChatColors, value: Option<String>) {
        match self {
            Self::Text => colors.text = value,
            Self::Background => colors.background = value,
            Self::Accent => colors.accent = value,
            Self::Bubble => colors.user_bubble = value,
            Self::CodeBackground => colors.code_block_background = value,
            Self::CodeText => colors.code_block_text = value,
            Self::InlineText => colors.inline_code_text = value,
            Self::InlineBackground => colors.inline_code_background = value,
        }
    }
}

const COLOR_FIELD_COUNT: usize = ColorField::ALL.len();

const PREVIEW_MARKDOWN: &str = "### A comfortable conversation\n\n\
     Read **clearly**, follow a [link](https://example.com), and keep `inline code` distinct.\n\n\
     中文字体预览 · The quick brown fox jumps over the lazy dog.\n\n\
     ```rust\n// Syntax highlighting stays enabled.\nlet message = \"Hello, Cypher!\";\nprintln!(\"{message}\");\n```";

fn preview_highlights(
    tree: &parser::BlockTree,
) -> HashMap<usize, Arc<cypher_syntax::HighlightedDocument>> {
    tree.blocks
        .iter()
        .enumerate()
        .filter_map(|(index, top)| {
            let parser::Block::CodeBlock { language, code } = &top.block else {
                return None;
            };
            cypher_syntax::highlight(cypher_syntax::HighlightRequest {
                source: code,
                path: None,
                fence_tag: language.as_deref(),
            })
            .ok()
            .map(|document| (index, Arc::new(document)))
        })
        .collect()
}

fn parsed_color(raw: &str) -> Result<Option<String>, &'static str> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    chat_style::normalize_hex(raw)
        .map(Some)
        .ok_or("Use a six-digit color, e.g. #AABBCC.")
}

pub(super) struct ChatStyleEditor {
    expanded: bool,
    palette: Appearance,
    follow_current_palette: bool,
    last_colors: ChatColors,
    color_inputs: [Entity<ComposerInput>; COLOR_FIELD_COUNT],
    color_pickers: [Entity<super::color_picker::ColorPicker>; COLOR_FIELD_COUNT],
    color_errors: [bool; COLOR_FIELD_COUNT],
    fonts: Arc<Vec<String>>,
    font_menu: Option<FontKind>,
    font_active: usize,
    font_search: Entity<ComposerInput>,
    font_scroll: gpui::ScrollHandle,
    font_focus: [FocusHandle; 2],
    error: Option<SharedString>,
    preview: Arc<parser::BlockTree>,
    preview_highlights: HashMap<usize, Arc<cypher_syntax::HighlightedDocument>>,
    _subscriptions: Vec<Subscription>,
}

impl ChatStyleEditor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let palette = Theme::of(cx).appearance;
        let colors = chat_style::settings(cx).colors(palette).clone();
        let color_inputs: [Entity<ComposerInput>; COLOR_FIELD_COUNT] =
            std::array::from_fn(|index| {
                cx.new(|cx| {
                    let mut input = ComposerInput::settings_field("Theme default", false, cx);
                    input.set_text(
                        ColorField::ALL[index]
                            .get(&colors)
                            .clone()
                            .unwrap_or_default(),
                        cx,
                    );
                    input
                })
            });
        let color_pickers = std::array::from_fn(|index| {
            cx.new(|cx| super::color_picker::ColorPicker::new(color_inputs[index].clone(), cx))
        });
        let font_search = cx
            .new(|cx| ComposerInput::with_context("Search installed fonts…", "PaletteSearch", cx));
        let mut subscriptions = Vec::new();
        for field in ColorField::ALL {
            subscriptions.push(cx.subscribe(
                &color_inputs[field.index()],
                move |this: &mut Self, _, event, cx| {
                    if matches!(
                        event,
                        ComposerInputEvent::Edited | ComposerInputEvent::Submitted
                    ) {
                        this.edit_color(field, cx);
                    }
                },
            ));
        }
        subscriptions.push(cx.subscribe(&font_search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.font_active = 0;
                this.font_scroll.scroll_to_item(0);
                cx.notify();
            }
        }));
        subscriptions.push(
            cx.observe_global::<ChatAppearanceState>(|this: &mut Self, cx| {
                if chat_style::settings(cx).colors(this.palette) != &this.last_colors {
                    this.sync_colors(cx);
                }
                cx.notify();
            }),
        );
        subscriptions.push(cx.observe_global::<Theme>(|this: &mut Self, cx| {
            if this.follow_current_palette {
                this.palette = Theme::of(cx).appearance;
                this.sync_colors(cx);
            }
            cx.notify();
        }));
        let fonts = cx
            .try_global::<ChatAppearanceState>()
            .map(|s| s.fonts.clone())
            .unwrap_or_default();
        let preview = Arc::new(parser::parse_full(PREVIEW_MARKDOWN));
        let preview_highlights = preview_highlights(&preview);
        Self {
            expanded: false,
            palette,
            follow_current_palette: true,
            last_colors: colors,
            color_inputs,
            color_pickers,
            color_errors: [false; COLOR_FIELD_COUNT],
            fonts,
            font_menu: None,
            font_active: 0,
            font_search,
            font_scroll: gpui::ScrollHandle::new(),
            font_focus: [cx.focus_handle(), cx.focus_handle()],
            error: None,
            preview,
            preview_highlights,
            _subscriptions: subscriptions,
        }
    }

    fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut ChatAppearance)) {
        let mut settings = chat_style::settings(cx).clone();
        edit(&mut settings);
        self.error = chat_style::set(settings, cx)
            .err()
            .map(|e| format!("Could not save chat appearance: {e}").into());
        self.last_colors = chat_style::settings(cx).colors(self.palette).clone();
        cx.notify();
    }

    fn sync_colors(&mut self, cx: &mut Context<Self>) {
        self.last_colors = chat_style::settings(cx).colors(self.palette).clone();
        self.color_errors = [false; COLOR_FIELD_COUNT];
        for field in ColorField::ALL {
            let value = field.get(&self.last_colors).clone().unwrap_or_default();
            if self.color_inputs[field.index()].read(cx).text() != value {
                self.color_inputs[field.index()].update(cx, |input, cx| input.set_text(value, cx));
            }
        }
    }

    fn edit_color(&mut self, field: ColorField, cx: &mut Context<Self>) {
        let value = parsed_color(self.color_inputs[field.index()].read(cx).text());
        self.color_errors[field.index()] = value.is_err();
        if let Ok(value) = value
            && field.get(chat_style::settings(cx).colors(self.palette)) != &value
        {
            let palette = self.palette;
            self.change(cx, |s| field.set(s.colors_mut(palette), value));
        }
        cx.notify();
    }

    fn number_row(
        &self,
        key: NumberSetting,
        settings: &ChatAppearance,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = key.get(settings);
        let (min, max, step) = key.range();
        let mut buttons = Vec::new();
        for (suffix, label, next, enabled) in [
            ("minus", "−", (value - step).max(min), value > min),
            ("plus", "+", (value + step).min(max), value < max),
        ] {
            buttons.push(
                widgets::ghost_action(theme)
                    .id(SharedString::from(format!("chat-{}-{suffix}", key.label())))
                    .aria_label(format!(
                        "{} {}",
                        if suffix == "minus" {
                            "Decrease"
                        } else {
                            "Increase"
                        },
                        key.label()
                    ))
                    .w(px(30.0))
                    .h(px(28.0))
                    .px_0()
                    .justify_center()
                    .when(!enabled, |el| el.opacity(0.3).cursor_default())
                    .when(enabled, |el| {
                        el.hover(|s| s.bg(theme.element_hover)).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.change(cx, |s| key.set(s, next))
                            }),
                        )
                    })
                    .child(label),
            );
        }
        let plus = buttons.pop().unwrap();
        let minus = buttons.pop().unwrap();
        super::setting_row()
            .child(div().flex_1().child(widgets::row_title(theme, key.label())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(minus)
                    .child(
                        div()
                            .w(px(70.0))
                            .text_center()
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .child(key.display(value)),
                    )
                    .child(plus),
            )
            .into_any_element()
    }

    fn font_choices(&self, cx: &gpui::App) -> Vec<Option<String>> {
        let query = self.font_search.read(cx).text().trim().to_lowercase();
        std::iter::once(None)
            .chain(self.fonts.iter().cloned().map(Some))
            .filter(|choice| {
                font_label(choice.as_deref())
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }

    fn choose_font(&mut self, font: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.font_menu.take() else {
            return;
        };
        self.change(cx, |s| match kind {
            FontKind::Body => s.font_family = font,
            FontKind::Code => s.code_font_family = font,
        });
        self.font_focus[kind as usize].focus(window, cx);
    }

    fn open_font(&mut self, kind: FontKind, window: &mut Window, cx: &mut Context<Self>) {
        self.font_menu = Some(kind);
        self.font_active = 0;
        self.font_scroll.scroll_to_item(0);
        self.font_search
            .update(cx, |input, cx| input.set_text("", cx));
        self.font_search.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn font_popup(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let choices = self.font_choices(cx);
        let count = choices.len();
        let selected = match self.font_menu {
            Some(FontKind::Code) => chat_style::settings(cx).code_font_family.clone(),
            _ => chat_style::settings(cx).font_family.clone(),
        };
        let rows = choices
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let label = font_label(value.as_deref());
                let is_selected = value == selected;
                div()
                    .id(("chat-font-choice", index))
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .bg(if index == self.font_active {
                        theme.element_active
                    } else {
                        gpui::transparent_black()
                    })
                    .hover(|s| s.bg(theme.element_hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.choose_font(value.clone(), window, cx)
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when(is_selected, |el| {
                        el.child(
                            crate::icons::icon(crate::icons::CHECK)
                                .size(px(12.0))
                                .text_color(theme.accent),
                        )
                    })
            })
            .collect::<Vec<_>>();
        let menu = div()
            .w(px(290.0))
            .rounded(px(10.0))
            .bg(theme.surface_overlay)
            .border_1()
            .border_color(theme.border_strong)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.font_menu = None;
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        if let Some(kind) = this.font_menu.take() {
                            this.font_focus[kind as usize].focus(window, cx);
                        }
                    }
                    "up" | "down" => {
                        let len = this.font_choices(cx).len();
                        if len > 0 {
                            this.font_active = if event.keystroke.key == "up" {
                                this.font_active.saturating_sub(1)
                            } else {
                                (this.font_active + 1).min(len - 1)
                            };
                            this.font_scroll.scroll_to_item(this.font_active);
                        }
                    }
                    "enter" => {
                        if let Some(value) = this.font_choices(cx).get(this.font_active).cloned() {
                            this.choose_font(value, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(div().p(px(10.0)).child(self.font_search.clone()))
            .child(
                div()
                    .id("chat-font-list")
                    .max_h(px(250.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.font_scroll)
                    .children(rows),
            )
            .when(count == 0, |el| {
                el.child(
                    div()
                        .p(px(12.0))
                        .text_color(theme.text_muted)
                        .child("No matching fonts."),
                )
            });
        crate::popover::anchored_menu_below("chat-font-popup", menu.into_any_element(), None)
    }

    fn font_row(
        &mut self,
        kind: FontKind,
        settings: &ChatAppearance,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (label, selected) = match kind {
            FontKind::Body => ("Message font", &settings.font_family),
            FontKind::Code => ("Code font", &settings.code_font_family),
        };
        let missing = selected
            .as_ref()
            .is_some_and(|name| !self.fonts.contains(name));
        let popup = (self.font_menu == Some(kind)).then(|| self.font_popup(theme, cx));
        super::setting_row()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(widgets::row_title(theme, label))
                    .when(missing, |el| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.warning_muted)
                                .child("Font unavailable; using the default."),
                        )
                    }),
            )
            .child(
                widgets::ghost_action(theme)
                    .id(("chat-font-trigger", kind as usize))
                    .aria_label(label)
                    .track_focus(&self.font_focus[kind as usize])
                    .relative()
                    .w(px(230.0))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_font(kind, window, cx)),
                    )
                    .on_key_down(cx.listener(
                        move |this, event: &gpui::KeyDownEvent, window, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                cx.stop_propagation();
                                this.open_font(kind, window, cx);
                            }
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(font_label(selected.as_deref())),
                    )
                    .child(
                        crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    )
                    .children(popup),
            )
            .into_any_element()
    }

    fn color_row(
        &self,
        field: ColorField,
        preview: &Theme,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let swatch = match field {
            ColorField::Text => preview.text,
            ColorField::Background => preview.bg,
            ColorField::Accent => preview.accent,
            ColorField::Bubble => preview.bg.blend(chat_style::bubble(preview)),
            ColorField::CodeBackground => preview.bg.blend(render::code_block_background(preview)),
            ColorField::CodeText => preview.code_block_text.unwrap_or(preview.text),
            ColorField::InlineText => render::inline_code_text(preview),
            ColorField::InlineBackground => preview.bg.blend(render::inline_code_wash(preview)),
        };
        let index = field.index();
        let input = self.color_inputs[index].clone();
        self.color_pickers[index].update(cx, |picker, cx| {
            picker.sync(swatch, self.palette.is_dark(), cx)
        });
        super::setting_row()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(widgets::row_title(theme, field.label()))
                    .when(matches!(field, ColorField::CodeText), |el| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child("Syntax highlighting stays unchanged."),
                        )
                    })
                    .when(self.color_errors[index], |el| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.danger_muted)
                                .child("Use #RRGGBB, or clear to follow the theme."),
                        )
                    }),
            )
            .child(self.color_pickers[index].clone())
            .child(
                div()
                    .id(("chat-color-field", index))
                    .w(px(152.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(if self.color_errors[index] {
                        theme.danger
                    } else {
                        theme.border
                    })
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        input.focus_handle(cx).focus(window, cx)
                    })
                    .child(self.color_inputs[index].clone()),
            )
            .child(
                widgets::ghost_action(theme)
                    .id(("chat-color-reset", index))
                    .child("Reset")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let palette = this.palette;
                        this.change(cx, |s| field.set(s.colors_mut(palette), None));
                        this.sync_colors(cx);
                    })),
            )
            .into_any_element()
    }
}

fn font_label(name: Option<&str>) -> SharedString {
    match name {
        None => "Default font".into(),
        Some(".SystemUIFont") => "System font".into(),
        Some(name) => name.to_string().into(),
    }
}

impl Render for ChatStyleEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let reset = widgets::ghost_action(&theme)
            .id("reset-chat-appearance")
            .child("Reset")
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.change(cx, |s| *s = ChatAppearance::default());
                this.sync_colors(cx);
            }))
            .into_any_element();
        let section = super::section_frame(&theme, false).child(super::section_header(
            &theme,
            "Chat interface",
            self.expanded,
            Some(reset),
            cx.listener(|this, _, _, cx| {
                this.expanded = !this.expanded;
                if !this.expanded {
                    this.font_menu = None;
                    for picker in &this.color_pickers {
                        picker.update(cx, |picker, cx| picker.close(cx));
                    }
                }
                cx.notify();
            }),
        ));
        if !self.expanded {
            return section.into_any_element();
        }
        let settings = chat_style::settings(cx).clone();
        let preview = chat_style::resolve(
            &settings,
            &crate::surface_style::apply_preset(
                Theme::for_appearance(self.palette),
                crate::surface_style::settings(cx)
                    .palette(self.palette)
                    .preset,
            ),
            0,
            &self.fonts,
        );
        let mut typography = super::content_group();
        for kind in [FontKind::Body, FontKind::Code] {
            typography = typography.child(self.font_row(kind, &settings, &theme, cx));
        }
        for key in NumberSetting::ALL {
            typography = typography.child(self.number_row(key, &settings, &theme, cx));
        }
        let mut palettes = div().flex().gap(px(8.0));
        for palette in [Appearance::Light, Appearance::Dark] {
            palettes = palettes.child(
                widgets::ghost_action(&theme)
                    .id(SharedString::from(format!("chat-palette-{palette:?}")))
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
                        this.follow_current_palette = palette == Theme::of(cx).appearance;
                        this.sync_colors(cx);
                        cx.notify();
                    })),
            );
        }
        let mut colors = super::content_group().child(palettes);
        for field in ColorField::ALL {
            colors = colors.child(self.color_row(field, &preview, &theme, cx));
        }
        if self.palette != theme.appearance {
            colors = colors.child(
                div().text_size(px(11.0)).text_color(theme.text_muted)
                    .child("Editing the other theme's palette. These colors apply when that theme is active."),
            );
        }
        let warnings = chat_style::contrast_warnings(&preview);
        if !warnings.is_empty() {
            colors = colors.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.warning_muted)
                    .child(format!(
                        "Low contrast (below 4.5:1): {}. Consider adjusting these colors.",
                        warnings.join(", ")
                    )),
            );
        }

        let options = render::RenderOptions::settled("chat-style-preview".into());
        let rendered = render::render_tree(&self.preview, &options, &preview, window, &|index| {
            self.preview_highlights.get(&index).cloned()
        });
        let live_preview = super::content_group().child(
            div()
                .id("chat-style-preview-scroll")
                .h(px(280.0))
                .overflow_y_scroll()
                .bg(preview.bg)
                .p(px(16.0))
                .child(
                    div()
                        .w_full()
                        .when(!settings.wide, |el| el.max_w(px(480.0)))
                        .mx_auto()
                        .flex()
                        .flex_col()
                        .gap(px(settings.message_spacing))
                        .child(
                            div().flex().justify_end().child(
                                div()
                                    .max_w(gpui::relative(0.8))
                                    .p(px(10.0))
                                    .rounded(px(10.0))
                                    .bg(chat_style::bubble(&preview))
                                    .font_family(preview.font_sans.clone())
                                    .text_size(px(preview.markdown.body_size))
                                    .line_height(px(preview.markdown.body_line_height))
                                    .text_color(preview.text)
                                    .child("Make this chat comfortable to read."),
                            ),
                        )
                        .child(rendered),
                ),
        );
        section.child(div().w_full().flex().flex_col().mt(px(super::SECTION_BODY_GAP))
            .child(widgets::page_subtitle(&theme, "Changes apply immediately to chats and their input boxes on this client only.").mt_0())
            .when_some(self.error.clone(), |el, error| el.child(widgets::error_strip(&theme, error)))
            .child(live_preview)
            .child(typography)
            .child(super::content_group().child(super::setting_row()
                .id("chat-wide-mode").cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| this.change(cx, |s| s.wide = !s.wide)))
                .child(div().flex_1().child(widgets::row_title(&theme, "Wide-screen mode"))
                    .child(div().mt(px(4.0)).text_size(px(12.0)).text_color(theme.text_muted)
                        .child("Expand messages and the composer to the available chat width. Side panels stay unchanged.")))
                .child(widgets::toggle_switch(&theme, settings.wide))))
            .child(colors))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_color_fields_have_unique_slots_and_independent_values() {
        let mut colors = ChatColors::default();
        assert_eq!(COLOR_FIELD_COUNT, 8);
        for (index, field) in ColorField::ALL.into_iter().enumerate() {
            assert_eq!(field.index(), index);
            field.set(&mut colors, Some(format!("#{index:06X}")));
        }
        for (index, field) in ColorField::ALL.into_iter().enumerate() {
            assert_eq!(
                field.get(&colors).as_deref(),
                Some(format!("#{index:06X}").as_str())
            );
        }
    }

    #[test]
    fn code_preview_contains_real_syntax_highlights() {
        let tree = parser::parse_full(PREVIEW_MARKDOWN);
        let documents = preview_highlights(&tree);
        assert!(!documents.is_empty());
        let kinds = documents
            .values()
            .flat_map(|doc| doc.lines.iter().flatten().map(|span| span.kind))
            .collect::<Vec<_>>();
        for kind in [
            cypher_syntax::HighlightKind::Keyword,
            cypher_syntax::HighlightKind::String,
            cypher_syntax::HighlightKind::Comment,
        ] {
            assert!(kinds.contains(&kind), "Preview must show {kind:?}");
        }
    }

    #[test]
    fn invalid_partial_colors_do_not_become_settings() {
        assert_eq!(parsed_color(""), Ok(None));
        assert_eq!(parsed_color(" aabbcc "), Ok(Some("#AABBCC".into())));
        for input in ["#", "#ABC", "#ABCDEF00", "#GGGGGG", "red"] {
            assert!(parsed_color(input).is_err());
        }
    }

    #[test]
    fn numeric_controls_cover_the_valid_settings_ranges() {
        for key in NumberSetting::ALL {
            let mut settings = ChatAppearance::default();
            let (min, max, step) = key.range();
            assert!(step > 0.0 && min <= key.get(&settings) && key.get(&settings) <= max);
            key.set(&mut settings, min);
            assert!((key.get(&settings.sanitized()) - min).abs() < 0.01);
            let mut settings = ChatAppearance::default();
            key.set(&mut settings, max);
            assert!((key.get(&settings.sanitized()) - max).abs() < 0.01);
        }
    }
}
