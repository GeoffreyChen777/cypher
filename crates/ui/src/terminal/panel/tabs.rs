//! Pure tab and stream helpers: panel height clamping, reconnect backoff,
//! drag-reorder math, the active tab after a reorder or close, the exit
//! notice and the base64 wire codec.

use base64::Engine as _;

use crate::prefs::{TERMINAL_MAX_VH, TERMINAL_MIN_HEIGHT};

// ---------------------------------------------------------------------------
// Pure logic (unit-tested)
// ---------------------------------------------------------------------------

/// Panel height clamp: 160 px … 55 % of the viewport.
pub fn clamp_terminal_height(height: f32, viewport_h: f32) -> f32 {
    let max = (viewport_h * TERMINAL_MAX_VH).max(TERMINAL_MIN_HEIGHT);
    if height.is_finite() {
        height.clamp(TERMINAL_MIN_HEIGHT, max)
    } else {
        TERMINAL_MIN_HEIGHT
    }
}

/// Reconnect backoff: 500 ms doubling to an 8 s ceiling.
pub fn backoff_ms(attempt: u32) -> u64 {
    (500u64 << attempt.min(4)).min(8_000)
}

/// Move a tab from `from` to `to` (indices into the same vec).
pub fn reorder_tabs<T>(tabs: &mut Vec<T>, from: usize, to: usize) {
    if from >= tabs.len() || to >= tabs.len() || from == to {
        return;
    }
    let tab = tabs.remove(from);
    tabs.insert(to, tab);
}

/// Where a drag hovering at `rel_x` inside the tab strip would land.
pub fn drop_index(rel_x: f32, tab_w: f32, count: usize) -> usize {
    if count == 0 || tab_w <= 0.0 {
        return 0;
    }
    ((rel_x / tab_w).floor().max(0.0) as usize).min(count - 1)
}

/// Sliding transform (in tab-width units) for tab `ix` while `from` is dragged
/// over `over`: tabs between the two shift one slot toward the vacated gap.
pub fn slide_offset(ix: usize, from: usize, over: usize) -> f32 {
    if from < over && ix > from && ix <= over {
        -1.0
    } else if over < from && ix >= over && ix < from {
        1.0
    } else {
        0.0
    }
}

/// Active index after a reorder commit.
pub fn active_after_reorder(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && to >= active {
        active - 1
    } else if from > active && to <= active {
        active + 1
    } else {
        active
    }
}

/// Merge the `targetDeviceId` passthrough into RPC params (no-op for chats on
/// the connected engine's own device).
pub(super) fn with_target(
    mut params: serde_json::Value,
    target: &Option<String>,
) -> serde_json::Value {
    if let (Some(target), Some(object)) = (target, params.as_object_mut()) {
        object.insert(
            "targetDeviceId".into(),
            serde_json::Value::String(target.clone()),
        );
    }
    params
}

/// Active index after closing `closed` (given the new, shorter length).
pub fn active_after_close(active: usize, closed: usize, len_after: usize) -> usize {
    let shifted = if closed < active { active - 1 } else { active };
    if len_after == 0 {
        0
    } else {
        shifted.min(len_after - 1)
    }
}

/// The `[process exited N]` trailer, dimmed.
pub fn exit_message(code: i32) -> Vec<u8> {
    format!("\r\n\x1b[90m[process exited {code}]\x1b[0m\r\n").into_bytes()
}

pub(super) fn decode_base64(data: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(data))
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "terminal: dropping undecodable data frame");
            Vec::new()
        })
}

pub(super) fn encode_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
