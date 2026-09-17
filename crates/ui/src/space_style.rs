//! Project card appearance: the glyph and colour a user picks for a Space.
//!
//! Both are stored on the synced row as short KEYS (`Space::icon`,
//! `Space::color`), never asset paths or hex, so a future palette change or
//! a device on an older build degrades to the default instead of breaking
//! the row. Unknown keys resolve to the defaults here.

use gpui::Hsla;

use crate::icons;
use crate::theme::Theme;

/// The pickable glyphs: (key, asset). The first entry is the default.
pub const SPACE_ICONS: &[(&str, &str)] = &[
    ("folder", icons::FOLDER),
    ("folder-files", icons::FOLDER_WITH_FILES),
    ("branch", icons::GIT_BRANCH),
    ("terminal", icons::TERMINAL),
    ("globe", icons::GLOBAL),
    ("document", icons::DOCUMENT),
    ("checklist", icons::CHECKLIST),
    ("widget", icons::WIDGET),
    ("tag", icons::TAG),
    ("cloud", icons::CLOUD),
    ("key", icons::KEY_MINIMALISTIC),
    ("monitor", icons::MONITOR),
    ("phone", icons::SMARTPHONE),
    ("chat", icons::CHAT_ROUND_LINE),
    ("star", icons::STAR),
    ("command", icons::COMMAND),
];

/// The pickable colours: (key, OKLCH hue). Chroma/lightness are fixed so
/// every swatch reads at the same weight in both appearances.
pub const SPACE_COLORS: &[(&str, f32)] = &[
    ("red", 25.0),
    ("orange", 55.0),
    ("amber", 80.0),
    ("green", 145.0),
    ("teal", 185.0),
    ("blue", 250.0),
    ("indigo", 277.0),
    ("purple", 310.0),
    ("pink", 350.0),
];

/// The glyph asset for a stored key (default folder for `None`/unknown).
pub fn space_icon(key: Option<&str>) -> &'static str {
    key.and_then(|key| SPACE_ICONS.iter().find(|(k, _)| *k == key))
        .map(|(_, asset)| *asset)
        .unwrap_or(icons::FOLDER)
}

/// The tint for a stored colour key (`None` for the default text colour or
/// an unknown key).
pub fn space_color(key: Option<&str>, theme: &Theme) -> Option<Hsla> {
    let hue = SPACE_COLORS.iter().find(|(k, _)| Some(*k) == key)?.1;
    Some(swatch(hue, theme))
}

/// One palette colour at display weight for the current appearance.
pub fn swatch(hue: f32, theme: &Theme) -> Hsla {
    let light = matches!(theme.appearance, crate::theme::Appearance::Light);
    crate::theme::oklch(if light { 0.58 } else { 0.72 }, 0.15, hue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_fall_back_to_defaults() {
        assert_eq!(space_icon(None), icons::FOLDER);
        assert_eq!(space_icon(Some("nope")), icons::FOLDER);
        assert_eq!(space_icon(Some("terminal")), icons::TERMINAL);
        let theme = Theme::for_appearance(crate::theme::Appearance::Dark);
        assert!(space_color(None, &theme).is_none());
        assert!(space_color(Some("nope"), &theme).is_none());
        assert!(space_color(Some("blue"), &theme).is_some());
        // Keys stay unique so the picker's checks are unambiguous.
        let mut keys: Vec<&str> = SPACE_ICONS.iter().map(|(k, _)| *k).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), SPACE_ICONS.len());
    }
}
