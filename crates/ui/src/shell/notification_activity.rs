//! Only coarse interaction age is reported. No keys, pointer coordinates,
//! window titles, prompts, or global OS activity are collected.
use gpui::{IntoElement, Styled};
use std::time::{Duration, Instant};

/// A paint-only, hitbox-free observer. Window event registration is illegal
/// during Render::render; a canvas also sees wheel events consumed by children.
pub fn scroll_observer(mut on_scroll: impl FnMut(&mut gpui::App) + 'static) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.on_mouse_event(move |_: &gpui::ScrollWheelEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture && window.is_window_active() {
                    on_scroll(cx);
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

pub struct DesktopActivity {
    pub client_id: String,
    interaction: Option<Instant>,
    sent: Option<Instant>,
    foreground: bool,
    chat: Option<String>,
    sequence: u64,
    dirty: bool,
}

impl Default for DesktopActivity {
    fn default() -> Self {
        Self {
            client_id: uuid::Uuid::new_v4().to_string(),
            interaction: None,
            sent: None,
            foreground: false,
            chat: None,
            sequence: 0,
            dirty: false,
        }
    }
}
impl DesktopActivity {
    pub fn interact(&mut self, now: Instant) -> bool {
        if self.interaction.is_none_or(|previous| {
            now.saturating_duration_since(previous) >= Duration::from_secs(10)
        }) {
            self.dirty = true;
        }
        self.interaction = Some(now);
        self.dirty
    }
    /// Is a repeat report — one that carries no state transition — worth a
    /// request?
    ///
    /// Only while this viewport is foreground AND on a chat. That is exactly
    /// when the Worker writes `target:{chatId}`, the record that holds a push
    /// back while the desk is watching that chat, and it refreshes on every
    /// report. Every other repeat re-sends a row no consumer reads: the
    /// Worker's `active()` requires `foreground`, and the only reader of the
    /// stored activity list is `iosViewingChat`, which matches
    /// `platform === "ios"` — desktop entries never suppress anything. Each
    /// such report cost one billable Durable Object request.
    ///
    /// Transitions are unaffected: `sample` reports those before consulting
    /// this, so going background, leaving a chat, or switching chats is still
    /// sent immediately.
    pub fn heartbeat_due(&self, now: Instant) -> bool {
        if !self.foreground || self.chat.is_none() {
            return false;
        }
        // The Worker lease is 45s; 15s keeps a 3x margin against a lost report.
        self.dirty
            || self
                .sent
                .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(15))
    }
    pub fn sample(
        &mut self,
        foreground: bool,
        chat: Option<String>,
        now: Instant,
    ) -> Option<serde_json::Value> {
        if foreground && !self.foreground {
            self.interact(now);
        }
        let changed = foreground != self.foreground || chat != self.chat;
        if !changed && !self.heartbeat_due(now) {
            return None;
        }
        self.foreground = foreground;
        self.chat = chat.clone();
        self.sent = Some(now);
        self.dirty = false;
        self.sequence += 1;
        let age = self
            .interaction
            .map(|at| {
                now.saturating_duration_since(at)
                    .as_millis()
                    .min(86_400_000) as u64
            })
            .unwrap_or(86_400_000);
        Some(serde_json::json!({
            "clientId": self.client_id, "sequence": self.sequence, "foreground": foreground, "chatId": chat, "interactionAgeMs": age
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn scroll_observer_registers_during_paint_not_render(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        cx.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(100.), gpui::px(100.)),
            |_, _| scroll_observer(|_| {}).into_any_element(),
        );
    }
    #[test]
    fn transitions_report_immediately_and_idle_repeats_do_not() {
        let now = Instant::now();
        let mut activity = DesktopActivity::default();
        // Becoming foreground is a transition.
        assert_eq!(
            activity.sample(true, None, now).unwrap()["interactionAgeMs"],
            0
        );
        assert!(
            activity
                .sample(true, None, now + Duration::from_secs(1))
                .is_none()
        );
        // Foreground but on no chat: a repeat would refresh nothing the Worker
        // reads, so it is not sent however long the window stays open.
        assert!(
            activity
                .sample(true, None, now + Duration::from_secs(121))
                .is_none(),
            "no chat open: repeats are not worth a request"
        );
        assert!(
            activity
                .sample(true, None, now + Duration::from_secs(3_600))
                .is_none()
        );
        // Going background IS a transition and still reports at once.
        assert_eq!(
            activity
                .sample(false, None, now + Duration::from_secs(3_601))
                .unwrap()["foreground"],
            false
        );
    }

    #[test]
    fn a_watched_chat_keeps_its_fifteen_second_lease() {
        let now = Instant::now();
        let mut activity = DesktopActivity::default();
        // Opening a chat is a transition.
        assert!(activity.sample(true, Some("chat".into()), now).is_some());
        assert!(!activity.heartbeat_due(now + Duration::from_secs(14)));
        assert!(
            activity.heartbeat_due(now + Duration::from_secs(15)),
            "the target record must stay inside the Worker's 45s lease"
        );
        assert!(
            activity
                .sample(true, Some("chat".into()), now + Duration::from_secs(15))
                .is_some()
        );
    }

    /// Quantifies the change on a realistic day, and pins it against
    /// regression. The old policy heartbeat unconditionally: 15s while
    /// foreground, 30s while background. Every one of those was a billable
    /// Durable Object request, and the Worker reads a repeat only while this
    /// viewport is foreground on a chat.
    #[test]
    fn a_realistic_day_costs_far_fewer_reports() {
        // 8h backgrounded, 1h foreground on the sessions list, 2h foreground
        // reading one chat. Sampled every second, as the render loop does.
        const BACKGROUND_S: u64 = 8 * 3600;
        const LIST_S: u64 = 3600;
        const CHAT_S: u64 = 2 * 3600;

        let start = Instant::now();
        let mut activity = DesktopActivity::default();
        let mut reports = 0;
        let mut at = 0;
        for (seconds, foreground, chat) in [
            (BACKGROUND_S, false, None),
            (LIST_S, true, None),
            (CHAT_S, true, Some("chat".to_string())),
        ] {
            for _ in 0..seconds {
                if activity
                    .sample(foreground, chat.clone(), start + Duration::from_secs(at))
                    .is_some()
                {
                    reports += 1;
                }
                at += 1;
            }
        }

        // Old policy: background 30s + foreground 15s, regardless of state.
        let old = BACKGROUND_S / 30 + (LIST_S + CHAT_S) / 15;
        assert_eq!(old, 1_680, "the policy this replaced");

        // New: the two state transitions that actually occur (the day starts
        // in the default background/no-chat state, so entering it is not a
        // transition), plus a 15s lease only while the chat is on screen.
        let transitions = 2;
        let chat_beats = (CHAT_S - 1) / 15;
        assert_eq!(
            reports,
            (transitions + chat_beats) as usize,
            "only transitions and the on-chat lease"
        );
        assert!(
            reports * 3 <= old as usize,
            "at least a 3x cut on this shape: {old} -> {reports}"
        );
        // Backgrounded and list time now cost nothing at all.
        assert_eq!(
            BACKGROUND_S / 30 + LIST_S / 15,
            1_200,
            "reports this removes outright"
        );
    }

    #[test]
    fn leaving_a_chat_or_backgrounding_stops_the_heartbeat() {
        let now = Instant::now();
        let mut activity = DesktopActivity::default();
        activity.sample(true, Some("chat".into()), now);
        // Closing the chat is a transition, and afterwards nothing repeats.
        assert!(
            activity
                .sample(true, None, now + Duration::from_secs(1))
                .is_some()
        );
        assert!(!activity.heartbeat_due(now + Duration::from_secs(600)));
        // Backgrounded while a chat is selected: `active()` needs foreground,
        // and `target:` is only written when foreground, so no repeats either.
        let mut background = DesktopActivity::default();
        background.sample(false, Some("chat".into()), now);
        assert!(!background.heartbeat_due(now + Duration::from_secs(600)));
    }
    #[test]
    fn actual_input_refreshes_age_and_chat_switch_is_immediate() {
        let now = Instant::now();
        let mut activity = DesktopActivity::default();
        activity.sample(true, None, now);
        activity.interact(now + Duration::from_secs(50));
        assert_eq!(
            activity
                .sample(true, Some("chat".into()), now + Duration::from_secs(51))
                .unwrap()["interactionAgeMs"],
            1000
        );
    }
}
