//! Context recovery in the transcript. A prompt the host sent again after a
//! failed turn is captioned above its bubble, in the steer caption's place
//! and voice; hovering the caption opens a card with the context the agent
//! received ahead of the prompt, the point the conversation restarted from,
//! and the trigger that set it off. The card is hoverable, so a long context
//! can be scrolled. While a recovery is under way, the transcript's trailer
//! says so (see [`recovery_trailer_text`]).

use super::*;

use crate::kit::popover;

use cypher_proto::{ContextRecovery, RecoveryProgress, RecoveryStage, RecoveryTriggerSource};

const CARD_WIDTH: f32 = 380.0;
/// The context scrolls past this height inside the card.
const CONTEXT_MAX_HEIGHT: f32 = 220.0;

/// Where the conversation restarted, as the card names it.
pub(super) fn restart_label(record: &ContextRecovery) -> String {
    match (&record.restart_after, &record.restart_preview) {
        (None, _) => "The beginning of the chat".into(),
        (Some(_), Some(preview)) => format!("After \u{201c}{preview}\u{201d}"),
        (Some(_), None) => "After the previous answer".into(),
    }
}

/// What set the recovery off, as the card names it.
pub(super) fn trigger_label(record: &ContextRecovery) -> String {
    let place = match record.matched_in {
        RecoveryTriggerSource::Response => "the agent\u{2019}s answer",
        RecoveryTriggerSource::Error => "an error",
    };
    format!("\u{201c}{}\u{201d} in {place}", record.trigger)
}

/// The caption's words: what happened, and which attempt it was.
pub(super) fn caption_label(record: &ContextRecovery) -> String {
    if record.attempt > 1 {
        format!("Restarted with context \u{b7} attempt {}", record.attempt)
    } else {
        "Restarted with context".into()
    }
}

/// The recovery trailer's word and its faint tail (elapsed time, the matched
/// phrase, the attempt), in the working trailer's shape.
pub(super) fn recovery_trailer_text(
    progress: &RecoveryProgress,
    now: chrono::DateTime<chrono::Utc>,
) -> (&'static str, String) {
    let word = match progress.stage {
        RecoveryStage::WritingContext => "Writing context to retry",
        RecoveryStage::Restarting => "Restarting with added context",
    };
    let elapsed = now
        .signed_duration_since(progress.started_at)
        .num_seconds()
        .max(0);
    let mut tail = format!(
        "{} \u{b7} matched \u{201c}{}\u{201d}",
        format_elapsed(elapsed),
        progress.trigger
    );
    if progress.attempt > 1 {
        tail.push_str(&format!(" \u{b7} attempt {}", progress.attempt));
    }
    (word, tail)
}

/// The caption over a recovered prompt's bubble: a muted restart glyph and
/// label, right-aligned and inset to the bubble's text like the steer
/// caption, brightening on hover. Hovering it opens the details card.
pub(super) fn recovery_caption(
    row_id: &SharedString,
    record: Arc<ContextRecovery>,
    pending: bool,
    theme: &Theme,
) -> AnyElement {
    let group = SharedString::from(format!("{row_id}-recovery-caption"));
    let label = caption_label(&record);
    let (muted, bright) = (theme.text_muted, theme.text);
    div()
        .w_full()
        .flex()
        .justify_end()
        .pr(px(12.0))
        .pb(px(4.0))
        .child(
            div()
                .id(SharedString::from(format!("{row_id}-recovery")))
                .group(group.clone())
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(muted)
                .group_hover(group.clone(), move |style| style.text_color(bright))
                .when(pending, |el| el.opacity(0.65))
                .child(
                    crate::kit::icons::icon(crate::kit::icons::RESTART)
                        .size(px(12.0))
                        .text_color(muted)
                        .group_hover(group, move |style| style.text_color(bright)),
                )
                .child(SharedString::from(label))
                .hoverable_tooltip(move |_, cx| {
                    let record = record.clone();
                    cx.new(|_| RecoveryCard { record }).into()
                })
                .tooltip_show_delay(Duration::from_millis(250)),
        )
        .into_any_element()
}

/// The hover card.
pub(super) struct RecoveryCard {
    record: Arc<ContextRecovery>,
}

fn section(theme: &Theme, label: &'static str, body: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text_faint)
                .child(label),
        )
        .child(body)
}

impl Render for RecoveryCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let record = &self.record;
        let body_text = |text: String| {
            div()
                .text_size(px(12.0))
                .line_height(px(17.0))
                .text_color(theme.text)
                .child(SharedString::from(text))
        };
        // The transcript's hover-card glass (the rail's prompt previews):
        // the popover tint over a backdrop blur.
        let card = popover::popover_card(&theme)
            .w(px(CARD_WIDTH))
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(section(
                &theme,
                "Restarted from",
                body_text(restart_label(record)),
            ))
            .child(section(
                &theme,
                "Triggered by",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(body_text(trigger_label(record)))
                    .when(!record.excerpt.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(11.5))
                                .line_height(px(16.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(record.excerpt.clone())),
                        )
                    }),
            ))
            .child(section(
                &theme,
                "Added context",
                div()
                    .id("recovery-card-context")
                    .max_h(px(CONTEXT_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .px(px(8.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(crate::kit::theme::hairline(0.06))
                    .bg(theme.ink(0.035))
                    .child(body_text(record.context.clone())),
            ))
            .when_some(record.model.clone(), |el, model| {
                el.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!("Written by {model}"))),
                )
            });
        motion::fade_quick(
            "recovery-card",
            div().child(crate::kit::frost::frosted(
                12.0,
                crate::kit::frost::MENU_BLUR,
                card,
            )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> ContextRecovery {
        ContextRecovery {
            context: "ctx".into(),
            trigger: "can't help".into(),
            matched_in: RecoveryTriggerSource::Response,
            excerpt: "Sorry, I can't help.".into(),
            restart_after: Some("a1".into()),
            restart_preview: Some("Here is the plan".into()),
            attempt: 1,
            model: None,
            failed_attempts: Vec::new(),
        }
    }

    #[test]
    fn the_card_names_where_the_chat_restarted() {
        let mut record = record();
        assert_eq!(
            restart_label(&record),
            "After \u{201c}Here is the plan\u{201d}"
        );
        record.restart_preview = None;
        assert_eq!(restart_label(&record), "After the previous answer");
        record.restart_after = None;
        assert_eq!(restart_label(&record), "The beginning of the chat");
    }

    #[test]
    fn the_caption_counts_attempts_after_the_first() {
        let mut record = record();
        assert_eq!(caption_label(&record), "Restarted with context");
        record.attempt = 2;
        assert_eq!(
            caption_label(&record),
            "Restarted with context \u{b7} attempt 2"
        );
    }

    #[test]
    fn the_trailer_says_what_the_recovery_is_doing() {
        let started = chrono::Utc::now();
        let mut progress = RecoveryProgress {
            stage: RecoveryStage::WritingContext,
            trigger: "can't help".into(),
            matched_in: RecoveryTriggerSource::Response,
            attempt: 1,
            started_at: started,
        };
        let (word, tail) = recovery_trailer_text(&progress, started + chrono::Duration::seconds(7));
        assert_eq!(word, "Writing context to retry");
        assert_eq!(tail, "7s \u{b7} matched \u{201c}can't help\u{201d}");
        progress.stage = RecoveryStage::Restarting;
        progress.attempt = 2;
        let (word, tail) = recovery_trailer_text(&progress, started);
        assert_eq!(word, "Restarting with added context");
        assert!(tail.ends_with("\u{b7} attempt 2"), "{tail}");
    }

    #[test]
    fn the_card_names_what_set_it_off() {
        let mut record = record();
        assert_eq!(
            trigger_label(&record),
            "\u{201c}can't help\u{201d} in the agent\u{2019}s answer"
        );
        record.matched_in = RecoveryTriggerSource::Error;
        record.trigger = "429".into();
        assert_eq!(trigger_label(&record), "\u{201c}429\u{201d} in an error");
    }
}
