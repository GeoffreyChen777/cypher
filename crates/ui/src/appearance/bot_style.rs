//! Agent bot avatars: a glyph (or the name's initial) on a tinted square.
//!
//! Like a project's style, a bot stores short KEYS (`Bot::icon`,
//! `Bot::color`); colours share the project palette, and an unknown key
//! falls back to the default — the bot's initial on a neutral square.

use gpui::{Div, Hsla, SharedString, div, prelude::*, px};

use crate::kit::icons::{self, icon};
use crate::kit::theme::Theme;

/// The pickable glyphs: (key, asset). `None` (the default) is the initial.
pub const BOT_ICONS: &[(&str, &str)] = &[
    ("bolt", icons::BOLT),
    ("lightbulb", icons::LIGHTBULB),
    ("code", icons::CODE),
    ("terminal", icons::TERMINAL),
    ("users", icons::USERS),
    ("pulse", icons::PULSE),
    ("book", icons::BOOK),
    ("chat", icons::CHAT_ROUND_LINE),
    ("checklist", icons::CHECKLIST),
    ("flag", icons::FLAG),
    ("hierarchy", icons::HIERARCHY),
    ("branch", icons::GIT_BRANCH),
    ("star", icons::STAR),
    ("globe", icons::GLOBAL),
    ("cloud", icons::CLOUD),
    ("key", icons::KEY_MINIMALISTIC),
    ("widget", icons::WIDGET),
];

/// The glyph asset for a stored key; `None` means "show the initial".
pub fn bot_icon(key: Option<&str>) -> Option<&'static str> {
    let key = key?;
    BOT_ICONS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, asset)| *asset)
}

/// The avatar's letter: the name's first letter or digit, uppercased.
pub fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "·".into())
}

/// A `size`-px avatar square. The tint colours the glyph; the square takes a
/// faint wash of it.
pub fn bot_avatar(
    name: &str,
    icon_key: Option<&str>,
    color_key: Option<&str>,
    size: f32,
    theme: &Theme,
) -> Div {
    let tint: Option<Hsla> = crate::appearance::space_style::space_color(color_key, theme);
    let fg = tint.unwrap_or(theme.text);
    let bg = match tint {
        Some(tint) => tint.opacity(0.18),
        None => crate::kit::theme::ink(0.08),
    };
    let square = div()
        .size(px(size))
        .flex_none()
        .rounded(px((size * 0.27).round()))
        .bg(bg)
        .flex()
        .items_center()
        .justify_center();
    match bot_icon(icon_key) {
        Some(asset) => square.child(icon(asset).size(px(size * 0.47)).text_color(fg)),
        None => square
            .text_size(px(size * 0.42))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(fg)
            .child(SharedString::from(initial(name))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_icons_fall_back_to_the_initial() {
        assert_eq!(bot_icon(None), None);
        assert_eq!(bot_icon(Some("nope")), None);
        assert_eq!(bot_icon(Some("code")), Some(icons::CODE));
        assert_eq!(initial("  release bot"), "R");
        assert_eq!(initial("构建机器人"), "构");
        assert_eq!(initial("--"), "·");
        let mut keys: Vec<&str> = BOT_ICONS.iter().map(|(k, _)| *k).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), BOT_ICONS.len());
    }
}
