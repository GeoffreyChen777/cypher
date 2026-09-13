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
    pub fn heartbeat_due(&self, now: Instant) -> bool {
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
    fn foreground_without_interaction_becomes_idle_and_background_reports_immediately() {
        let now = Instant::now();
        let mut activity = DesktopActivity::default();
        assert_eq!(
            activity.sample(true, None, now).unwrap()["interactionAgeMs"],
            0
        );
        assert!(
            activity
                .sample(true, None, now + Duration::from_secs(1))
                .is_none()
        );
        assert_eq!(
            activity
                .sample(true, None, now + Duration::from_secs(121))
                .unwrap()["interactionAgeMs"],
            121_000
        );
        assert_eq!(
            activity
                .sample(false, None, now + Duration::from_secs(122))
                .unwrap()["foreground"],
            false
        );
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
