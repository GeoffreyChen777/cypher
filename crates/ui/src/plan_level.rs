//! A plan's permission level, as its card and its page pick it
//! (docs/design/agent-bots.md §6.6): the three levels of `/permissions`,
//! named and toned by its badge (neutral ink, amber at Full access). On a
//! plan still to approve a pick is held (`AppState::plan_level_picks`)
//! until Approve confirms it (`resolveBotPlan` `level`); once approved it
//! is sent at once (`setBotWorkLevel`), and the plan's record says it.

use cypher_proto::{BotWork, SandboxLevel};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use crate::composer::slash_menu::{self, Action, Badge, Facts, Tone};
use crate::kit::icons::{self, icon};
use crate::kit::popover;
use crate::kit::theme::Theme;
use crate::transcript::bot_plan::{PlanPhase, plan_phase};

/// What picking a level on a plan does.
#[derive(Debug, Clone, PartialEq)]
pub enum PickEffect {
    /// Held until Approve confirms it (a plan still to approve).
    Hold(SandboxLevel),
    /// Sent now: the Mutate params.
    Send(serde_json::Value),
}

/// Whether a plan's level can still change: before it is decided, and
/// from its approval until it ends.
pub fn pickable(work: &BotWork) -> bool {
    matches!(
        plan_phase(work),
        PlanPhase::Awaiting | PlanPhase::Starting | PlanPhase::Running
    )
}

/// What picking `level` on `work` does; `None` when nothing changes (the
/// level it already runs at, or a plan past changing).
pub fn pick_effect(work: &BotWork, level: SandboxLevel) -> Option<PickEffect> {
    match plan_phase(work) {
        PlanPhase::Awaiting => Some(PickEffect::Hold(level)),
        PlanPhase::Starting | PlanPhase::Running => (level != work.effective_level()).then(|| {
            PickEffect::Send(serde_json::json!({
                "op": "setBotWorkLevel", "workId": work.id, "level": level,
            }))
        }),
        _ => None,
    }
}

/// Pick `level` on `work`: a held pick lands in the state at once; what
/// to send comes back for the view's own Mutate.
pub fn pick(
    state: &gpui::Entity<crate::state::AppState>,
    work: &BotWork,
    level: SandboxLevel,
    cx: &mut gpui::App,
) -> Option<serde_json::Value> {
    match pick_effect(work, level)? {
        PickEffect::Hold(level) => {
            let id = work.id.clone();
            state.update(cx, |state, cx| {
                state.plan_level_picks.insert(id, level);
                cx.notify();
            });
            None
        }
        PickEffect::Send(params) => Some(params),
    }
}

/// The level the picker shows: a pick held on a plan still to approve,
/// else the plan's own.
pub fn shown(work: &BotWork, held: Option<SandboxLevel>) -> SandboxLevel {
    match held {
        Some(level) if plan_phase(work).awaiting() => level,
        _ => work.effective_level(),
    }
}

/// Approve's params: with the held pick, if the owner made one (else the
/// engine confirms the level it was proposed at).
pub fn approve_params(work_id: &str, held: Option<SandboxLevel>) -> serde_json::Value {
    let mut params = serde_json::json!({
        "op": "resolveBotPlan", "workId": work_id, "approve": true,
    });
    if let Some(level) = held {
        params["level"] = serde_json::json!(level);
    }
    params
}

/// The workspace setting for `level` as new plans' default: none for Edit
/// project, so it follows a later default (like the other plan limits).
pub fn default_setting(level: SandboxLevel) -> Option<SandboxLevel> {
    (level != SandboxLevel::WorkspaceWrite).then_some(level)
}

/// The owner's words a proposal's raised level came from, while it waits
/// on them (a pick doesn't hide them: the card keeps its height).
pub fn quote(work: &BotWork) -> Option<&str> {
    let quote = work.level_quote.as_deref()?.trim();
    (!quote.is_empty() && plan_phase(work).awaiting()).then_some(quote)
}

/// The level's name and tone, as `/permissions` badges it.
pub fn badge(level: SandboxLevel) -> Badge {
    let facts = Facts {
        permission: Some(level),
        ..Default::default()
    };
    slash_menu::action_badge(Action::Permissions, &facts).unwrap_or(Badge {
        label: level.label().to_string(),
        tone: Tone::Neutral,
    })
}

fn tone_color(tone: Tone, theme: &Theme) -> gpui::Hsla {
    match tone {
        Tone::Warning => theme.warning,
        _ => theme.text_muted,
    }
}

/// The picker's trigger: a quiet chip with the level's name, amber at Full
/// access. The caller adds the click and, while open, the menu.
pub fn trigger(
    id: impl Into<SharedString>,
    level: SandboxLevel,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let badge = badge(level);
    let color = tone_color(badge.tone, theme);
    let key = id.clone();
    div()
        .id(id)
        .flex_none()
        .relative()
        .h(px(24.0))
        .px(px(7.0))
        .rounded(px(6.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .cursor_pointer()
        .text_size(px(12.0))
        .text_color(color)
        .bg(crate::kit::motion::hover_blend(
            &key,
            crate::kit::theme::wash(0.0),
            crate::kit::theme::wash(0.07),
        ))
        .on_hover(crate::kit::motion::hover_listener(key.clone()))
        .child(
            icon(icons::SHIELD_CHECK)
                .size(px(12.0))
                .flex_none()
                .text_color(color),
        )
        .child(SharedString::from(badge.label))
        .child(
            icon(icons::ALT_ARROW_DOWN)
                .size(px(10.0))
                .flex_none()
                .text_color(theme.text_muted),
        )
}

/// The picker's menu: the three levels with what each allows, a check on
/// the one shown. `pick` gets the row's level; `close` runs on a press
/// outside.
pub fn menu<V: 'static>(
    key: &str,
    current: SandboxLevel,
    closing: Option<std::time::Instant>,
    theme: &Theme,
    cx: &mut Context<V>,
    pick: impl Fn(&mut V, SandboxLevel, &mut Context<V>) + Clone + 'static,
    close: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> AnyElement {
    let card = popover::popover_card(theme)
        .id(SharedString::from(format!("{key}-menu")))
        .w(px(272.0))
        .on_mouse_down_out(cx.listener(move |this, _, _, cx| close(this, cx)))
        .children(crate::pickers::PERMISSION_LEVELS.iter().map(|entry| {
            let on = entry.level == current;
            let level = entry.level;
            let row_key = format!("{key}-{}", entry.value);
            let pick = pick.clone();
            let color = tone_color(badge(level).tone, theme);
            popover::menu_row(theme, on, row_key.clone())
                .id(SharedString::from(row_key))
                .items_start()
                .on_click(cx.listener(move |this, _, _, cx| pick(this, level, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .when(level == SandboxLevel::DangerFullAccess, |el| {
                                    el.text_color(color)
                                })
                                .child(SharedString::from(entry.label)),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(entry.description)),
                        ),
                )
                .child(div().w(px(14.0)).flex_none().pt(px(2.0)).when(on, |el| {
                    el.child(icon(icons::CHECK).size(px(13.0)).text_color(theme.text))
                }))
        }))
        .into_any_element();
    popover::anchored_menu_below(SharedString::from(format!("{key}-layer")), card, closing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::{PlanDecision, WorkState};

    fn work(state: WorkState) -> BotWork {
        let mut work = crate::test_fixtures::bot_work("w1", "b1");
        work.state = state;
        work
    }

    #[test]
    fn a_proposed_plan_holds_the_pick_for_approve() {
        let mut proposed = work(WorkState::Proposed);
        proposed.proposed_level = Some(SandboxLevel::ReadOnly);
        assert_eq!(shown(&proposed, None), SandboxLevel::ReadOnly);
        assert_eq!(
            pick_effect(&proposed, SandboxLevel::DangerFullAccess),
            Some(PickEffect::Hold(SandboxLevel::DangerFullAccess))
        );
        assert_eq!(
            shown(&proposed, Some(SandboxLevel::DangerFullAccess)),
            SandboxLevel::DangerFullAccess
        );
        // Approve confirms the pick; without one the proposal's level.
        assert_eq!(
            approve_params("w1", Some(SandboxLevel::DangerFullAccess)),
            serde_json::json!({
                "op": "resolveBotPlan", "workId": "w1", "approve": true,
                "level": "danger-full-access",
            })
        );
        assert_eq!(
            approve_params("w1", None),
            serde_json::json!({ "op": "resolveBotPlan", "workId": "w1", "approve": true })
        );
    }

    #[test]
    fn a_running_plan_sends_the_pick_at_once() {
        let mut running = work(WorkState::Running);
        running.level = Some(SandboxLevel::WorkspaceWrite);
        assert_eq!(
            pick_effect(&running, SandboxLevel::ReadOnly),
            Some(PickEffect::Send(serde_json::json!({
                "op": "setBotWorkLevel", "workId": "w1", "level": "read-only",
            })))
        );
        // The level it runs at already: nothing to send.
        assert_eq!(pick_effect(&running, SandboxLevel::WorkspaceWrite), None);
        // A pick held before approval no longer shows: the record does.
        assert_eq!(
            shown(&running, Some(SandboxLevel::ReadOnly)),
            SandboxLevel::WorkspaceWrite
        );
        running.level = Some(SandboxLevel::ReadOnly);
        assert_eq!(shown(&running, None), SandboxLevel::ReadOnly);

        // Approved, not started yet: sent too.
        let mut starting = work(WorkState::Proposed);
        starting.decision = Some(PlanDecision {
            approve: true,
            at: 1,
        });
        assert!(matches!(
            pick_effect(&starting, SandboxLevel::ReadOnly),
            Some(PickEffect::Send(_))
        ));
    }

    #[test]
    fn an_ended_or_rejected_plan_is_past_changing() {
        for state in [WorkState::Done, WorkState::Failed, WorkState::Stopped] {
            let ended = work(state);
            assert!(!pickable(&ended));
            assert_eq!(pick_effect(&ended, SandboxLevel::ReadOnly), None);
        }
        let mut rejected = work(WorkState::Proposed);
        rejected.decision = Some(PlanDecision {
            approve: false,
            at: 1,
        });
        assert!(!pickable(&rejected));
        let mut stopping = work(WorkState::Running);
        stopping.stop_requested_at = Some(1);
        assert!(!pickable(&stopping));
    }

    #[test]
    fn the_quote_shows_while_the_plan_waits() {
        let mut proposed = work(WorkState::Proposed);
        proposed.proposed_level = Some(SandboxLevel::DangerFullAccess);
        proposed.level_quote = Some("run it with full access".into());
        assert_eq!(quote(&proposed), Some("run it with full access"));
        proposed.decision = Some(PlanDecision {
            approve: true,
            at: 1,
        });
        assert_eq!(quote(&proposed), None);
    }

    #[test]
    fn the_workspace_default_is_stored_only_when_it_differs() {
        let mut plans = cypher_proto::PlanSettings::default();
        assert_eq!(plans.level(), SandboxLevel::WorkspaceWrite);
        plans.level = default_setting(SandboxLevel::ReadOnly);
        assert_eq!(plans.level(), SandboxLevel::ReadOnly);
        assert!(!plans.is_default());
        plans.level = default_setting(SandboxLevel::WorkspaceWrite);
        assert!(plans.is_default());
        assert_eq!(serde_json::json!(plans), serde_json::json!({}));
        plans.level = default_setting(SandboxLevel::DangerFullAccess);
        assert_eq!(
            serde_json::json!(plans),
            serde_json::json!({ "level": "danger-full-access" })
        );
    }

    #[test]
    fn levels_read_as_the_permissions_badge() {
        assert_eq!(badge(SandboxLevel::ReadOnly).label, "Ask before edits");
        assert_eq!(badge(SandboxLevel::WorkspaceWrite).tone, Tone::Neutral);
        let full = badge(SandboxLevel::DangerFullAccess);
        assert_eq!(
            (full.label.as_str(), full.tone),
            ("Full access", Tone::Warning)
        );
    }
}
