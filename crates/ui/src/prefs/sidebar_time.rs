//! The sidebar's "last active" window: the slider's stops, the free-text
//! duration the field next to it accepts, and the mapping between the two.
//! A window is a whole number of minutes; `None` keeps every session.

/// The slider's stops (minutes), left to right. Past the last one the
/// slider reads "All".
pub const STOPS: [u32; 11] = [
    60,
    2 * 60,
    3 * 60,
    6 * 60,
    12 * 60,
    DAY,
    2 * DAY,
    3 * DAY,
    7 * DAY,
    14 * DAY,
    30 * DAY,
];

const DAY: u32 = 24 * 60;

/// The longest window a hand-typed value keeps (a year).
pub const MAX_MINS: u32 = 365 * DAY;

/// Slider positions: one per stop, plus "All" at the far right.
const POSITIONS: f32 = STOPS.len() as f32;

/// Heal a loaded or typed window into range; zero means no window.
pub fn clamp(mins: Option<u32>) -> Option<u32> {
    mins.filter(|&m| m > 0).map(|m| m.min(MAX_MINS))
}

/// Where a window sits on the slider (0..=1). Values between stops sit
/// between them on a log scale; anything past the last stop pins to it.
pub fn slider_position(mins: Option<u32>) -> f32 {
    let Some(mins) = mins else {
        return 1.0;
    };
    let last = STOPS.len() - 1;
    let index = match STOPS.iter().position(|&stop| stop >= mins) {
        None => last as f32,
        Some(0) => 0.0,
        Some(ix) => {
            let (lo, hi) = (STOPS[ix - 1] as f32, STOPS[ix] as f32);
            (ix - 1) as f32 + (mins as f32 / lo).ln() / (hi / lo).ln()
        }
    };
    index / POSITIONS
}

/// The stop nearest a slider position (0..=1); the far right is "All".
pub fn from_slider(position: f32) -> Option<u32> {
    let index = (position.clamp(0.0, 1.0) * POSITIONS).round() as usize;
    STOPS.get(index).copied()
}

/// Compact display: `45m`, `12h`, `1h 30m`, `3d`, `1d 6h`; `None` is "All".
pub fn format(mins: Option<u32>) -> String {
    let Some(mins) = mins else {
        return "All".into();
    };
    let (days, hours, minutes) = (mins / DAY, mins % DAY / 60, mins % 60);
    let parts: Vec<String> = [(days, "d"), (hours, "h"), (minutes, "m")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect();
    parts.join(" ")
}

/// Parse a typed window: one or more `<number><unit>` terms (`90m`, `1.5h`,
/// `2 days`, `1d 6h`, `1w`), units m/h/d/w (and their spelled-out and
/// Chinese forms). A bare number is hours. Empty, `all` or `0` clears the
/// window (`Some(None)`); anything unreadable is `None`.
pub fn parse(text: &str) -> Option<Option<u32>> {
    let text = text.trim().to_lowercase();
    if text.is_empty() || matches!(text.as_str(), "all" | "any" | "0" | "全部") {
        return Some(None);
    }
    let mut total = 0.0_f64;
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let number: f64 = rest[..digits].parse().ok()?;
        rest = rest[digits..].trim_start();
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit() || c == '.' || c.is_whitespace() || c == ',')
            .unwrap_or(rest.len());
        let scale = unit_minutes(&rest[..unit_len])?;
        total += number * scale;
        rest = rest[unit_len..].trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    }
    let mins = total.round();
    if !mins.is_finite() || mins < 0.0 {
        return None;
    }
    Some(clamp(Some(mins.min(MAX_MINS as f64) as u32)))
}

/// Minutes per unit; an empty unit reads as hours.
fn unit_minutes(unit: &str) -> Option<f64> {
    Some(match unit {
        "m" | "min" | "mins" | "minute" | "minutes" | "分" | "分钟" => 1.0,
        "" | "h" | "hr" | "hrs" | "hour" | "hours" | "时" | "小时" => 60.0,
        "d" | "day" | "days" | "天" | "日" => f64::from(DAY),
        "w" | "wk" | "wks" | "week" | "weeks" | "周" | "星期" => f64::from(7 * DAY),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_windows() {
        assert_eq!(parse("12h"), Some(Some(12 * 60)));
        assert_eq!(parse("12"), Some(Some(12 * 60)));
        assert_eq!(parse(" 90m "), Some(Some(90)));
        assert_eq!(parse("1.5h"), Some(Some(90)));
        assert_eq!(parse("2 days"), Some(Some(2 * DAY)));
        assert_eq!(parse("1d 6h"), Some(Some(DAY + 6 * 60)));
        assert_eq!(parse("1w"), Some(Some(7 * DAY)));
        assert_eq!(parse("3天"), Some(Some(3 * DAY)));
        assert_eq!(parse("半"), None);
        assert_eq!(parse("All"), Some(None));
        assert_eq!(parse(""), Some(None));
        assert_eq!(parse("0h"), Some(None));
        assert_eq!(parse("12x"), None);
        assert_eq!(parse("h"), None);
        assert_eq!(parse("9999w"), Some(Some(MAX_MINS)));
    }

    #[test]
    fn formats_compactly_and_round_trips() {
        for (mins, text) in [
            (Some(45), "45m"),
            (Some(90), "1h 30m"),
            (Some(12 * 60), "12h"),
            (Some(3 * DAY), "3d"),
            (Some(DAY + 6 * 60), "1d 6h"),
            (None, "All"),
        ] {
            assert_eq!(format(mins), text);
            assert_eq!(parse(text), Some(mins));
        }
    }

    #[test]
    fn slider_snaps_to_stops_and_places_values_between_them() {
        assert_eq!(from_slider(0.0), Some(60));
        assert_eq!(from_slider(1.0), None);
        for (ix, &stop) in STOPS.iter().enumerate() {
            let at = slider_position(Some(stop));
            assert!((at - ix as f32 / POSITIONS).abs() < 1e-6);
            assert_eq!(from_slider(at), Some(stop));
        }
        assert_eq!(slider_position(None), 1.0);
        // 18h sits between 12h and 1d; under an hour pins left, past 30d
        // pins to the last stop (never onto "All").
        let (twelve, day) = (slider_position(Some(12 * 60)), slider_position(Some(DAY)));
        let between = slider_position(Some(18 * 60));
        assert!(twelve < between && between < day);
        assert_eq!(slider_position(Some(5)), 0.0);
        assert_eq!(
            slider_position(Some(90 * DAY)),
            slider_position(Some(30 * DAY))
        );
    }
}
