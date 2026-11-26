//! **Needs you** (§13.1): everything in a workspace that waits on its
//! owner, in one place. The sidebar card lists the items, most pressing
//! first, in the amber the sidebar gives a session awaiting input; its rows
//! and the pill over the chat's composer open the one page that answers
//! them.
//!
//! The page stacks a card per item, each naming where it comes from and
//! answered in place: the workspace's own question (answered in its chat),
//! an action or a plan to approve, a permission prompt or a question from
//! a session the workspace runs (a plan's worker, a routine), and an
//! answer to review before it is sent. Decisions only: a failed routine is
//! state, marked on its routine, and a held memory write asks in the chat
//! that made it.
//!
//! A decision is one owner field on a record (a `Mutate`) or an answer
//! queued on the asking session, so every count follows wherever it was
//! made, and a decided card leaves the page.

use super::board::{card_corner, card_glyph, card_item};
use super::items::ago;
use super::panel::{PanelHead, PanelItem};
use super::*;
use crate::command_outcome::{AnswerOutcome, Outcome, answer_outcome};
use crate::composer::permission_card::{self, FactKind, OptionRole};
use crate::kit::theme::MonoStyled;
use cypher_proto::{NeedsYou, UserInputQuestion};

/// Rows the card shows before "N more". More than the other cards' short
/// lists: every one of these waits on the owner.
const CARD_ROWS: usize = 5;

/// The lines of a tool's input or a change a permission card shows before
/// it says how many more there are.
const BLOCK_LINES: usize = 14;

/// The gap between the page's cards (the gap between the chat and the
/// panel) and a card's own padding.
const CARD_GAP: f32 = 8.0;
const CARD_PAD: f32 = 14.0;
/// How far a card's shadow reaches past its sides.
const SHADOW_ROOM: f32 = 4.0;

/// The gap between a card's buttons, a text button's side padding, and an
/// icon button's side.
const BUTTON_GAP: f32 = 6.0;
const BUTTON_PAD: f32 = 12.0;
const ICON_BUTTON: f32 = 28.0;

/// The key of the workspace's own question among the items.
const CHAT_KEY: &str = "chat";

/// What a Needs you item is, with what its card draws.
enum NeedKind {
    /// The workspace's own question, waiting in its chat.
    Chat {
        preview: String,
    },
    Plan {
        work_id: String,
        sub: String,
        summary: Option<String>,
    },
    Approval {
        id: String,
        reason: String,
    },
    /// A session the workspace runs is asking. `chat_id` is the chat that
    /// asks (the session, or a subagent's child chat under it): the card
    /// reads and answers its question there.
    Asking {
        chat_id: String,
    },
    Review {
        id: String,
    },
}

/// One Needs you item as the card's row and the page's card draw it.
struct NeedItem {
    /// Stable across frames: the row's and the card's key.
    key: String,
    glyph: &'static str,
    /// What is asked, in a line.
    title: String,
    /// Where it comes from: "Fix invoice rounding › Migrate tables".
    source: String,
    at: Option<chrono::DateTime<Utc>>,
    /// Its host device is offline: the question waits for it.
    waiting: bool,
    kind: NeedKind,
}

/// "Plan · 4 steps · web-api · Studio": the place when every step shares it.
pub(super) fn plan_line(state: &crate::state::AppState, work: &cypher_proto::BotWork) -> String {
    let places: Vec<Option<String>> = work
        .steps
        .iter()
        .map(|step| {
            let space = state.space_row(step.project.as_deref()?)?;
            Some(match state.device_name(&space.device_id) {
                Some(device) => format!("{} · {device}", space.display_name()),
                None => space.display_name().to_string(),
            })
        })
        .collect();
    let common = places
        .first()
        .cloned()
        .flatten()
        .filter(|first| places.iter().all(|p| p.as_deref() == Some(first.as_str())));
    let workspace_limit = state
        .bots
        .iter()
        .find(|b| b.id == work.bot_id)
        .map_or(240, |b| b.plans.time_limit_minutes());
    crate::transcript::bot_plan::plan_sub(
        work.steps.len(),
        common.as_deref(),
        crate::transcript::bot_plan::own_limit(work, workspace_limit),
    )
}

/// Who a session the workspace runs belongs to, for its card's source: the
/// plan that started it or the routine it runs for.
fn session_owner(state: &crate::state::AppState, chat: &cypher_proto::Chat) -> Option<String> {
    let records = &state.bot_records;
    if let Some((work, _)) = records.started_by(&chat.id) {
        return Some(transcript::single_line(&work.title));
    }
    let routine_id = match chat.bot.as_ref().map(|marker| &marker.thread) {
        Some(
            cypher_proto::BotThread::Routine { routine_id }
            | cypher_proto::BotThread::RoutineSession { routine_id },
        ) => routine_id,
        _ => return None,
    };
    records
        .routines
        .iter()
        .find(|r| &r.id == routine_id)
        .map(|r| format!("{} · routine", transcript::single_line(&r.name)))
}

fn need_items(state: &crate::state::AppState, bot: &Bot, needs: Vec<Need>) -> Vec<NeedItem> {
    let records = &state.bot_records;
    let name = transcript::single_line(&bot.name);
    needs
        .into_iter()
        .filter_map(|need| match need {
            Need::Chat => {
                let chat = state.chats.iter().find(|c| c.id == bot.chat_id)?;
                Some(NeedItem {
                    key: CHAT_KEY.into(),
                    glyph: icons::CHAT_ROUND_LINE,
                    title: format!("{name} asked you something"),
                    source: name.clone(),
                    at: chat.last_message_at,
                    waiting: false,
                    kind: NeedKind::Chat {
                        preview: chat
                            .last_message_preview
                            .as_deref()
                            .map(transcript::single_line)
                            .unwrap_or_default(),
                    },
                })
            }
            Need::Record(NeedsYou::Plan(id)) => {
                let work = records.work.iter().find(|w| w.id == id)?;
                Some(NeedItem {
                    key: format!("plan-{id}"),
                    glyph: icons::CHECKLIST,
                    title: transcript::single_line(&work.title),
                    source: "Plan to approve".into(),
                    at: Some(super::items::at(work.created_at)),
                    waiting: false,
                    kind: NeedKind::Plan {
                        sub: plan_line(state, work),
                        summary: work
                            .summary
                            .as_deref()
                            .map(transcript::single_line)
                            .filter(|s| !s.is_empty()),
                        work_id: id,
                    },
                })
            }
            Need::Record(NeedsYou::Approval(id)) => {
                let approval = records.approvals.iter().find(|a| a.id == id)?;
                let from = approval
                    .work_id
                    .as_deref()
                    .and_then(|id| records.work.iter().find(|w| w.id == id))
                    .map(|w| transcript::single_line(&w.title))
                    .unwrap_or_else(|| name.clone());
                Some(NeedItem {
                    key: format!("approval-{id}"),
                    glyph: icons::SHIELD_CHECK,
                    title: transcript::single_line(&approval.action),
                    source: from,
                    at: Some(super::items::at(approval.created_at)),
                    waiting: false,
                    kind: NeedKind::Approval {
                        reason: approval.reason.clone(),
                        id,
                    },
                })
            }
            Need::Record(NeedsYou::Asking { chat_id, .. }) => {
                let chat = state.chats.iter().find(|c| c.id == chat_id)?;
                // Where the question is open: the session or a child chat
                // under it; offline when its device is.
                let question = state.plan_question(&chat_id, Utc::now());
                let waiting = question.as_ref().is_some_and(|q| q.offline);
                let asker = question.map_or_else(|| chat_id.clone(), |q| q.chat_id);
                let title = transcript::single_line(chat.title.as_deref().unwrap_or("New session"));
                let project = chat
                    .space_id
                    .as_deref()
                    .and_then(|id| state.space_row(id))
                    .map(|s| s.display_name().to_string());
                // The owner, then the session (a routine's own thread is
                // named like its routine: once is enough), else its project.
                let source = match (session_owner(state, chat), project) {
                    (Some(owner), _) if chat.is_bot() => owner,
                    (Some(owner), _) => format!("{owner} › {title}"),
                    (None, Some(project)) => format!("{title} · {project}"),
                    (None, None) => title.clone(),
                };
                Some(NeedItem {
                    key: format!("ask-{chat_id}"),
                    glyph: icons::QUESTION_CIRCLE,
                    title,
                    source,
                    at: chat.last_message_at,
                    waiting,
                    kind: NeedKind::Asking { chat_id: asker },
                })
            }
            Need::Record(NeedsYou::Review(id)) => {
                let exchange = records.exchanges.iter().find(|e| e.id == id)?;
                let peer = records
                    .connections
                    .iter()
                    .find(|c| c.id == exchange.connection_id)
                    .map(|c| c.peer.name.clone())
                    .unwrap_or_else(|| "a connected workspace".into());
                Some(NeedItem {
                    key: format!("review-{id}"),
                    glyph: super::items::REVIEW_GLYPH,
                    title: transcript::single_line(&exchange.question),
                    source: format!("Answer to {peer}, to review"),
                    at: Some(super::items::at(exchange.asked_at)),
                    waiting: false,
                    kind: NeedKind::Review { id },
                })
            }
        })
        .collect()
}

/// What a question asks, as its card says it: the prompt's headline and
/// the context under it.
fn question_text(question: &UserInputQuestion) -> String {
    if question.question.trim().is_empty() {
        question.header.clone()
    } else {
        question.question.clone()
    }
}

/// The click that answers a session's question with one of its options.
fn answer_click(
    chat_id: String,
    request_id: String,
    question_id: String,
    label: String,
    cx: &mut Context<Shell>,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static + use<> {
    let shell = cx.weak_entity();
    move |_, _, cx| {
        shell
            .update(cx, |this, cx| {
                this.answer_session(
                    chat_id.clone(),
                    request_id.clone(),
                    question_id.clone(),
                    label.clone(),
                    cx,
                )
            })
            .ok();
    }
}

/// A card's quiet button (Reject, Deny, Details): text that washes on hover.
fn quiet_button(key: &str, label: &str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    popover::btn_ghost(theme, label, key.to_string())
        .id(SharedString::from(key.to_string()))
        .flex_none()
        .py(px(5.0))
}

/// A card's outlined button (a broader approval, an answer to pick).
fn outline_button(key: &str, label: &str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(key.to_string()))
        .flex_none()
        .max_w_full()
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .text_size(px(13.0))
        .text_color(theme.text)
        .cursor_pointer()
        .bg(motion::hover_blend(
            key,
            crate::kit::theme::ink(0.02),
            crate::kit::theme::ink(0.07),
        ))
        .on_hover(motion::hover_listener(key.to_string()))
        .child(SharedString::from(label.to_string()))
}

/// A card's filled button: the decision it most likely wants.
fn primary_button(key: &str, label: &str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    popover::btn_primary(theme, label)
        .id(SharedString::from(key.to_string()))
        .flex_none()
        .py(px(5.0))
}

/// The row a card's buttons sit in, at its foot under a hairline: at the
/// right, wrapping.
fn card_actions() -> gpui::Div {
    div()
        .mt(px(12.0))
        .pt(px(10.0))
        .border_t_1()
        .border_color(crate::kit::theme::hairline(0.07))
        .flex()
        .flex_row()
        .flex_wrap()
        .justify_end()
        .items_center()
        .gap(px(BUTTON_GAP))
}

/// A label's width in a card's button.
fn label_width(label: &str, weight: gpui::FontWeight, theme: &Theme, window: &Window) -> f32 {
    let run = gpui::TextRun {
        len: label.len(),
        font: gpui::Font {
            weight,
            ..gpui::font(theme.font_sans.clone())
        },
        color: theme.text,
        background_color: None,
        underline: None,
        strikethrough: None,
        font_size: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(label.to_string()),
        px(13.0),
        &[run],
        None,
    );
    f32::from(line.width).ceil()
}

/// Which of a permission prompt's options draw as icons so that all of them
/// share one line `room` wide: none while their labels fit, then the
/// refusals, then the broader approvals from the broadest in. The narrowest
/// approval always keeps its label. `widths` are the buttons with labels.
fn folded_options(roles: &[OptionRole], widths: &[f32], room: f32) -> Vec<bool> {
    let mut folded = vec![false; roles.len()];
    let fits = |folded: &[bool]| {
        let buttons: f32 = folded
            .iter()
            .zip(widths)
            .map(|(folded, width)| if *folded { ICON_BUTTON } else { *width })
            .sum();
        // The buttons and the space that parts refusals from approvals.
        buttons + BUTTON_GAP * roles.len() as f32 <= room
    };
    if fits(&folded) {
        return folded;
    }
    for (ix, role) in roles.iter().enumerate() {
        folded[ix] = matches!(role, OptionRole::Deny | OptionRole::Stop);
    }
    // The extension lists approvals from the narrowest out.
    for ix in (0..roles.len()).rev() {
        if fits(&folded) {
            break;
        }
        folded[ix] |= roles[ix] == OptionRole::Secondary;
    }
    folded
}

/// The glyph a folded approval draws: what it lasts for.
fn approval_glyph(label: &str) -> &'static str {
    if label.starts_with("Always") {
        icons::SHIELD_CHECK
    } else if label.ends_with("this chat") {
        icons::CHAT_ROUND_LINE
    } else {
        icons::CHECK
    }
}

/// A permission option folded to its icon, its label in the tooltip: a
/// cross for Deny, the stop square for Deny and stop, an outlined glyph for
/// a broader approval.
fn icon_option(
    key: &str,
    label: &str,
    role: OptionRole,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let tip = SharedString::from(label.to_string());
    let base = div()
        .id(SharedString::from(key.to_string()))
        .flex_none()
        .size(px(ICON_BUTTON))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_hover(motion::hover_listener(key.to_string()))
        .tooltip(move |_, cx| {
            cx.new(|_| super::super::session::FindTooltip(tip.clone()))
                .into()
        });
    // A refusal's label is bare text; alone, its icon needs a plate to read
    // as a button.
    match role {
        OptionRole::Stop => base
            .bg(motion::hover_blend(
                key,
                theme.danger.opacity(0.08),
                theme.danger.opacity(0.16),
            ))
            .child(div().size(px(9.0)).rounded(px(2.0)).bg(theme.danger)),
        OptionRole::Deny => base
            .bg(motion::hover_blend(
                key,
                crate::kit::theme::ink(0.045),
                crate::kit::theme::ink(0.09),
            ))
            .child(
                icon(icons::CLOSE)
                    .size(px(12.0))
                    .text_color(motion::hover_blend(
                        key,
                        theme.text.opacity(0.7),
                        theme.text,
                    )),
            ),
        OptionRole::Secondary | OptionRole::Primary => base
            .border_1()
            .border_color(theme.border)
            .bg(motion::hover_blend(
                key,
                crate::kit::theme::ink(0.02),
                crate::kit::theme::ink(0.07),
            ))
            .child(
                icon(approval_glyph(label))
                    .size(px(14.0))
                    .text_color(theme.text.opacity(0.85)),
            ),
    }
}

/// A line of card copy under its title.
fn card_copy(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .mt(px(4.0))
        .text_size(px(12.5))
        .line_height(px(18.0))
        .text_color(theme.text.opacity(0.85))
        .child(text.into())
}

/// A quiet note in place of a card's buttons (an answer on its way).
/// A question whose session's device is offline: it stays open and is
/// answered once the device is back (docs/design/plan-asks.md).
const WAITING_FOR_DEVICE: &str = "Waiting for its device";

fn card_note(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .mt(px(8.0))
        .text_size(px(12.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// The prompt of the question `request_id` names in a transcript, resolved
/// or not.
fn input_prompt(
    transcript: &[cypher_doc::SessionMessageEntry],
    request_id: &str,
) -> Option<String> {
    transcript.iter().rev().find_map(|entry| {
        entry.parts.iter().find_map(|part| match part {
            cypher_doc::MessagePart::Input {
                request_id: rid,
                questions,
                ..
            } if rid == request_id => questions.first().map(question_text),
            _ => None,
        })
    })
}

/// A question answered from a Needs you card: in which chat, and as which
/// `RespondInput` command (`None` until the queue call answers).
#[derive(Debug, Clone)]
pub(super) struct SentAnswer {
    chat_id: String,
    command_id: Option<String>,
}

impl Shell {
    fn current_need_items(&self, bot: &Bot, cx: &App) -> Vec<NeedItem> {
        let state = self.state.read(cx);
        need_items(state, bot, bot_needs(state, bot, Utc::now()))
    }

    /// How many items wait on the owner beside the chat of the workspace
    /// on the board: everything but the workspace's own question, which
    /// already stands over its composer.
    pub(in crate::shell) fn board_needs_count(&self, cx: &App) -> usize {
        let state = self.state.read(cx);
        current_bot(&state.bots, self.settings.botboard_bot.as_deref()).map_or(0, |bot| {
            bot_needs(state, bot, Utc::now())
                .iter()
                .filter(|need| **need != Need::Chat)
                .count()
        })
    }

    /// Decide a record's card on the page (a plan, an action): the
    /// decision is the record's, and its card leaves once it lands.
    pub(in crate::shell) fn decide_need(
        &mut self,
        params: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        self.board.needs_decided = true;
        self.board_mutate(params, cx);
    }

    /// Slide the panel shut once the owner has decided the Needs you page's
    /// last item: the page has nothing left to do. Runs on every main-state
    /// change, so it closes when the decision lands, not when it is sent
    /// (an item that fails or is replaced keeps the page open). The page
    /// emptied elsewhere, without a decision here, stays open.
    pub(in crate::shell) fn close_needs_when_done(&mut self, cx: &mut Context<Self>) {
        if !self.board.needs_decided || self.board.panel != Some(PanelItem::NeedsYou) {
            return;
        }
        let done = {
            let state = self.state.read(cx);
            current_bot(&state.bots, self.settings.botboard_bot.as_deref()).is_none_or(|bot| {
                need_items(state, bot, bot_needs(state, bot, Utc::now())).is_empty()
            })
        };
        if done {
            self.close_panel(cx);
        }
    }

    /// Open the Needs you page on the card `key` (a row, the pill). The
    /// workspace's own question is answered in its chat: its row goes there.
    pub(in crate::shell) fn open_need(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if key.as_deref() == Some(CHAT_KEY) {
            self.close_panel(cx);
            self.tiles.focus_pending = true;
            cx.notify();
            return;
        }
        self.show_panel(PanelItem::NeedsYou, cx);
        self.board.needs_focus = key;
        cx.notify();
    }

    /// The sidebar card; `None` when there is nothing to show. Its rows
    /// name what waits; the page answers.
    pub(super) fn render_needs_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let items = self.current_need_items(bot, cx);
        if items.is_empty() {
            return None;
        }
        let now = Utc::now();
        let total = items.len();
        let header = self.card_header(
            "needs",
            "bot-needs-header",
            "Needs you",
            total,
            Some(super::board::HeaderAction::Page(PanelItem::NeedsYou)),
            Some(theme.warning),
            theme,
            cx,
        );
        let shown = if self.list_expanded("needs") {
            total
        } else {
            CARD_ROWS
        };
        let mut rows: Vec<AnyElement> = items
            .into_iter()
            .take(shown)
            .map(|item| {
                let key = item.key.clone();
                card_item(
                    &format!("bot-need-{}", item.key),
                    Some(card_glyph(item.glyph, theme.warning)),
                    item.title,
                    if item.waiting {
                        Some(card_corner(WAITING_FOR_DEVICE, None, theme))
                    } else {
                        item.at
                            .map(|at| card_corner(format_time_ago(at, now), None, theme))
                    },
                    false,
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_need(Some(key.clone()), cx)))
                .into_any_element()
            })
            .collect();
        // The rest come in in place, like any card's.
        rows.extend(self.more_row("needs".into(), total.saturating_sub(CARD_ROWS), theme, cx));
        Some(self.fold_card("needs", header, rows, theme))
    }

    /// The page: a card per item, answered in place. The cards stand in
    /// the panel's column on their own, shaped like the chat's card, with no
    /// frame or title around them; the page scrolls itself, so a row can
    /// bring its card into view.
    pub(super) fn render_needs_page(
        &mut self,
        bot: &Bot,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let items = self.current_need_items(bot, cx);
        let card_bg = crate::appearance::chat_style::panel_background(
            crate::appearance::chat_style::settings(cx),
            theme,
            true,
        );
        let card = || {
            div()
                .flex_none()
                .rounded(px(PANEL_CORNER_RADIUS))
                .bg(card_bg)
                .shadow_sm()
                .flex()
                .flex_col()
        };
        // The sessions this page looks into are the ones still asking.
        let asking: Vec<&str> = items
            .iter()
            .filter_map(|item| match &item.kind {
                NeedKind::Asking { chat_id } => Some(chat_id.as_str()),
                _ => None,
            })
            .collect();
        self.board
            .peeks
            .retain(|chat_id, _| asking.contains(&chat_id.as_str()));
        if items.is_empty() {
            self.board.needs_focus = None;
            return div().size_full().child(
                card()
                    .items_center()
                    .px(px(CARD_PAD))
                    .py(px(36.0))
                    .child(parts::panel_empty(
                        theme,
                        icons::CHECK,
                        "You're all caught up",
                        "Approvals, permission prompts, questions and answers to review land here.",
                    )),
            );
        }
        // A row asked for one card: bring it into view.
        let focus = self.board.needs_focus.take();
        if let Some(ix) = focus
            .as_ref()
            .and_then(|key| items.iter().position(|item| &item.key == key))
        {
            self.board.needs_scroll.scroll_to_item(ix);
        }
        // What a card's buttons have to share.
        let room = self.panel_width_in(window) - 2.0 * CARD_PAD;
        let now = Utc::now();
        let cards: Vec<AnyElement> = items
            .into_iter()
            .map(|item| {
                let content = self.need_content(&item, room, theme, window, cx);
                let head = self.need_head(&item, now, theme, cx);
                card()
                    .p(px(CARD_PAD))
                    .pt(px(12.0))
                    .child(head)
                    .child(
                        div()
                            .mt(px(6.0))
                            .text_size(px(13.5))
                            .line_height(px(19.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(match &item.kind {
                                // A session's card is titled by its question.
                                NeedKind::Asking { .. } => content.title.clone(),
                                _ => item.title.clone(),
                            })),
                    )
                    .child(content.body)
                    .into_any_element()
            })
            .collect();
        // The list clips what scrolls out of it, so it reaches past the
        // cards' sides and foot by the room their shadows take.
        div().size_full().relative().child(
            div()
                .id("bot-needs-page")
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(-SHADOW_ROOM))
                .right(px(-SHADOW_ROOM))
                .overflow_y_scroll()
                .track_scroll(&self.board.needs_scroll)
                .px(px(SHADOW_ROOM))
                .pb(px(SHADOW_ROOM))
                .flex()
                .flex_col()
                .gap(px(CARD_GAP))
                .children(cards),
        )
    }

    /// A card's first line: where the item comes from, when, and for a
    /// session the way into it.
    fn need_head(
        &self,
        item: &NeedItem,
        now: chrono::DateTime<Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = match &item.kind {
            NeedKind::Asking { chat_id } => {
                let chat_id = chat_id.clone();
                Some(
                    super::items::icon_button(
                        format!("bot-need-open-{}", item.key),
                        icons::CHAT_ROUND_LINE,
                        "Open the session",
                        theme,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)),
                    ),
                )
            }
            _ => None,
        };
        div()
            .h(px(22.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.0))
            .child(
                icon(item.glyph)
                    .size(px(13.0))
                    .flex_none()
                    .text_color(theme.warning),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(item.source.clone())),
            )
            .children(
                if item.waiting {
                    Some(WAITING_FOR_DEVICE.to_string())
                } else {
                    item.at.map(|at| format_time_ago(at, now))
                }
                .map(|corner| {
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(corner))
                }),
            )
            .children(open)
    }

    /// What a card says under its title and the buttons that decide it.
    fn need_content(
        &mut self,
        item: &NeedItem,
        room: f32,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> NeedContent {
        let key = &item.key;
        match &item.kind {
            NeedKind::Chat { preview } => NeedContent::of(
                div()
                    .flex()
                    .flex_col()
                    .when(!preview.is_empty(), |el| {
                        el.child(card_copy(preview.clone(), theme).line_clamp(3))
                    })
                    .child(
                        card_actions().child(
                            primary_button(&format!("{key}-answer"), "Answer in the chat", theme)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_panel(cx);
                                    this.tiles.focus_pending = true;
                                    cx.notify();
                                })),
                        ),
                    ),
            ),
            NeedKind::Plan {
                work_id,
                sub,
                summary,
            } => {
                let (approve, reject, details) =
                    (work_id.clone(), work_id.clone(), work_id.clone());
                // Its level, picked here as on its card and page.
                let work = self
                    .state
                    .read(cx)
                    .bot_records
                    .work
                    .iter()
                    .find(|w| w.id == *work_id)
                    .cloned();
                let picker = work
                    .and_then(|work| self.level_picker(&format!("{key}-level"), &work, theme, cx));
                NeedContent::of(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .mt(px(1.0))
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(sub.clone())),
                        )
                        .children(
                            summary
                                .clone()
                                .map(|summary| card_copy(summary, theme).line_clamp(3)),
                        )
                        .child(
                            card_actions()
                                .child(
                                    quiet_button(&format!("{key}-details"), "Details", theme)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.show_panel(PanelItem::Plan(details.clone()), cx)
                                        })),
                                )
                                .child(div().flex_1())
                                .children(picker)
                                .child(
                                    quiet_button(&format!("{key}-reject"), "Reject", theme)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide_need(
                                                serde_json::json!({
                                                    "op": "resolveBotPlan",
                                                    "workId": reject,
                                                    "approve": false,
                                                }),
                                                cx,
                                            )
                                        })),
                                )
                                .child(
                                    primary_button(&format!("{key}-approve"), "Approve", theme)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let held = this
                                                .state
                                                .read(cx)
                                                .plan_level_picks
                                                .get(&approve)
                                                .copied();
                                            this.decide_need(
                                                crate::plan_level::approve_params(&approve, held),
                                                cx,
                                            )
                                        })),
                                ),
                        ),
                )
            }
            NeedKind::Approval { id, reason } => {
                let (yes, no) = (id.clone(), id.clone());
                NeedContent::of(
                    div()
                        .flex()
                        .flex_col()
                        .child(card_copy(reason.clone(), theme))
                        .child(
                            card_actions()
                                .child(
                                    quiet_button(&format!("{key}-decline"), "Decline", theme)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide_need(
                                                serde_json::json!({
                                                    "op": "resolveBotApproval",
                                                    "approvalId": no,
                                                    "approve": false,
                                                }),
                                                cx,
                                            )
                                        })),
                                )
                                .child(
                                    primary_button(&format!("{key}-approve"), "Approve", theme)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.decide_need(
                                                serde_json::json!({
                                                    "op": "resolveBotApproval",
                                                    "approvalId": yes,
                                                    "approve": true,
                                                }),
                                                cx,
                                            )
                                        })),
                                ),
                        ),
                )
            }
            NeedKind::Review { id } => {
                let review = id.clone();
                NeedContent::of(div().flex().flex_col().child(card_actions().child(
                    primary_button(&format!("{key}-review"), "Review the answer", theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.show_panel(PanelItem::Review(review.clone()), cx)
                        }),
                    ),
                )))
            }
            NeedKind::Asking { chat_id } => {
                self.asking_content(key, chat_id, room, theme, window, cx)
            }
        }
    }

    /// A session's question as its card: a permission prompt with what it
    /// asks about and the extension's own options, a question with its
    /// answers to pick, or the way into the session for anything that takes
    /// more than a pick.
    fn asking_content(
        &mut self,
        key: &str,
        chat_id: &str,
        room: f32,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> NeedContent {
        let peek = self.peek(chat_id, false, cx);
        let (pending, loaded, sent) = {
            let state = peek.read(cx);
            let pending = crate::composer::pending_input_request(&state.transcript);
            // What the answer sent from here came to (one rule,
            // `command_outcome::answer_outcome`): for the open question, or,
            // once none is open, for one answered here and since resolved
            // (turned down when another device answered first).
            let sent = match &pending {
                Some((request_id, questions)) => self.board.answered.get(request_id).map(|sent| {
                    let outcome = Outcome::of(state.commands(), sent.command_id.as_deref());
                    (
                        questions.first().map(question_text),
                        answer_outcome(Some(outcome), false),
                    )
                }),
                None => self
                    .board
                    .answered
                    .iter()
                    .filter(|(_, sent)| sent.chat_id == chat_id)
                    .find_map(|(request_id, sent)| {
                        let outcome = Outcome::of(state.commands(), sent.command_id.as_deref());
                        let shown = answer_outcome(Some(outcome), true);
                        matches!(shown, AnswerOutcome::NoLongerWaiting(_))
                            .then(|| (input_prompt(&state.transcript, request_id), shown))
                    }),
            };
            (pending, !state.transcript.is_empty(), sent)
        };
        if let Some((prompt, shown)) = sent
            && let Some(text) = shown.text()
        {
            let headline = prompt
                .map(|prompt| crate::composer::split_question_context(&prompt).0)
                .unwrap_or_else(|| "Waiting on you".into());
            return NeedContent {
                title: headline,
                body: card_note(text, theme),
            };
        }
        let Some((request_id, questions)) = pending else {
            return NeedContent {
                title: if loaded {
                    "Nothing to answer right now".into()
                } else {
                    "Loading the question…".into()
                },
                body: div(),
            };
        };
        let Some(question) = questions.first().cloned() else {
            return NeedContent {
                title: "Waiting on you".into(),
                body: div(),
            };
        };
        let prompt = question_text(&question);
        let answer = |label: &str, cx: &mut Context<Self>| {
            answer_click(
                chat_id.to_string(),
                request_id.clone(),
                question.id.clone(),
                label.to_string(),
                cx,
            )
        };
        let single = questions.len() == 1;
        // A permission prompt: what it asks about, then the extension's
        // options in its order, weighed as the question card weighs them.
        if single && let Some(request) = permission_card::parse(&question, &prompt) {
            let rows: Vec<gpui::Div> = request
                .rows()
                .map(|fact| {
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(px(10.0))
                        .child(
                            div()
                                .flex_none()
                                .w(px(60.0))
                                .text_size(px(11.5))
                                .line_height(px(17.0))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(fact.kind.label().unwrap_or_default())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(if fact.kind.monospace() { 11.5 } else { 12.5 }))
                                .line_height(px(17.0))
                                .text_color(theme.text)
                                .when(fact.kind.monospace(), |el| el.mono(theme))
                                .child(SharedString::from(fact.value.clone())),
                        )
                })
                .collect();
            let sentences: Vec<gpui::Div> = request
                .sentences()
                .map(|fact| {
                    let glyph = match fact.kind {
                        FactKind::Review => icons::LIGHTBULB,
                        FactKind::Network => icons::GLOBAL,
                        _ => icons::INFO_CIRCLE,
                    };
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(px(7.0))
                        .child(
                            icon(glyph)
                                .size(px(12.0))
                                .mt(px(3.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.0))
                                .line_height(px(17.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(fact.value.clone())),
                        )
                })
                .collect();
            let block = request.block.as_ref().map(|(label, text)| {
                let lines: Vec<&str> = text.lines().collect();
                let shown = lines
                    .iter()
                    .take(BLOCK_LINES)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                let more = lines.len().saturating_sub(BLOCK_LINES);
                div()
                    .mt(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(label.clone())),
                    )
                    .child(
                        div()
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(9.0))
                            .bg(crate::kit::theme::ink(0.035))
                            .overflow_hidden()
                            .text_size(px(11.5))
                            .line_height(px(16.0))
                            .text_color(theme.text.opacity(0.9))
                            .mono(theme)
                            .child(SharedString::from(shown)),
                    )
                    .when(more > 0, |el| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(format!(
                                    "{more} more line{} in the session",
                                    if more == 1 { "" } else { "s" }
                                ))),
                        )
                    })
            });
            // Weighed and placed as the session's own card does it, on one
            // line: the refusals at the left, the approvals at the right
            // with the narrowest one last. Where the card is too narrow for
            // every label, options fold to icons.
            let roles = permission_card::option_roles(&question.options);
            let widths: Vec<f32> = question
                .options
                .iter()
                .zip(&roles)
                .map(|(label, role)| {
                    let (weight, border) = match role {
                        OptionRole::Primary => (gpui::FontWeight::MEDIUM, 0.0),
                        OptionRole::Secondary => (gpui::FontWeight::NORMAL, 2.0),
                        _ => (gpui::FontWeight::NORMAL, 0.0),
                    };
                    label_width(label, weight, theme, window) + 2.0 * BUTTON_PAD + border
                })
                .collect();
            let folded = folded_options(&roles, &widths, room);
            let (mut refusals, mut broader, mut primary) = (Vec::new(), Vec::new(), None);
            for (ix, (label, role)) in question.options.iter().zip(roles).enumerate() {
                let button_key = format!("{key}-option-{ix}");
                let button = if folded[ix] {
                    icon_option(&button_key, label, role, theme)
                } else {
                    match role {
                        OptionRole::Primary => primary_button(&button_key, label, theme),
                        OptionRole::Secondary => outline_button(&button_key, label, theme),
                        OptionRole::Deny => quiet_button(&button_key, label, theme),
                        OptionRole::Stop => quiet_button(&button_key, label, theme)
                            .text_color(theme.danger.opacity(0.85)),
                    }
                };
                let button = button.on_click(answer(label, cx)).into_any_element();
                match role {
                    OptionRole::Deny | OptionRole::Stop => refusals.push(button),
                    OptionRole::Secondary => broader.push(button),
                    OptionRole::Primary => primary = Some(button),
                }
            }
            broader.reverse();
            let buttons = card_actions()
                .justify_start()
                .children(refusals)
                .child(div().flex_1())
                .children(broader)
                .children(primary);
            let body = div()
                .flex()
                .flex_col()
                .when(!rows.is_empty(), |el| {
                    el.child(
                        div()
                            .mt(px(8.0))
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(9.0))
                            .bg(crate::kit::theme::ink(0.035))
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .children(rows),
                    )
                })
                .when(!sentences.is_empty(), |el| {
                    el.child(
                        div()
                            .mt(px(8.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .children(sentences),
                    )
                })
                .children(block)
                .child(buttons);
            return NeedContent {
                title: request.question,
                body,
            };
        }
        let (headline, context) = crate::composer::split_question_context(&prompt);
        let context = context.map(|context| card_copy(context, theme));
        let simple = single && !question.multi_select && !question.options.is_empty();
        let body = if simple {
            // One question with answers to pick: each is a button.
            let options: Vec<AnyElement> = question
                .options
                .iter()
                .enumerate()
                .map(|(ix, label)| {
                    outline_button(&format!("{key}-option-{ix}"), label, theme)
                        .on_click(answer(label, cx))
                        .into_any_element()
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .children(context)
                .child(card_actions().justify_start().children(options))
        } else {
            // Several questions, several answers or one to type: the
            // session's own question card takes them.
            let open = chat_id.to_string();
            div()
                .flex()
                .flex_col()
                .children(context)
                .child(card_note(
                    if single {
                        "This one takes more than a pick."
                    } else {
                        "It asks several things at once."
                    },
                    theme,
                ))
                .child(
                    card_actions().child(
                        primary_button(&format!("{key}-open"), "Answer in the session", theme)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.open_chat(open.clone(), cx)),
                            ),
                    ),
                )
        };
        NeedContent {
            title: headline,
            body,
        }
    }

    /// Answer a worker's question with one option, the way its question
    /// card would (`RespondInput` queued on its session).
    fn answer_session(
        &mut self,
        chat_id: String,
        request_id: String,
        question_id: String,
        label: String,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let command = cypher_doc::SessionCommandPayload::RespondInput {
            request_id: request_id.clone(),
            answers: vec![cypher_proto::UserInputAnswer {
                question_id,
                labels: vec![label],
            }],
        };
        let Ok(command) = serde_json::to_value(&command) else {
            return;
        };
        self.board.answered.insert(
            request_id.clone(),
            SentAnswer {
                chat_id: chat_id.clone(),
                command_id: None,
            },
        );
        self.board.needs_decided = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let params = serde_json::json!({ "chatId": chat_id, "command": command });
            let reply = engine.client().call(methods::QUEUE_COMMAND, params).await;
            this.update(cx, |this, cx| {
                match reply {
                    // The card follows this command's outcome from here on.
                    Ok(reply) => {
                        if let Some(sent) = this.board.answered.get_mut(&request_id) {
                            sent.command_id = reply["commandId"].as_str().map(str::to_string);
                        }
                    }
                    Err(err) => {
                        this.board.answered.remove(&request_id);
                        this.sidebar.notice = Some(format!("Answer failed: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// An answer the workspace drafted for a connected workspace's
    /// question, waiting for the owner: Decline and Send at the very top,
    /// who asks, the question as a quote, and the draft to edit. Once it is
    /// decided (here or on another device) the question lives on its
    /// connection's page, which this then shows.
    pub(super) fn render_review_panel(
        &mut self,
        bot: &Bot,
        exchange_id: &str,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let now = Utc::now();
        let (exchange, peer) = {
            let state = self.state.read(cx);
            let exchange = state
                .bot_records
                .exchanges
                .iter()
                .find(|e| e.id == exchange_id)?
                .clone();
            let peer = state
                .bot_records
                .connections
                .iter()
                .find(|c| c.id == exchange.connection_id)
                .map(|c| c.peer.clone());
            (exchange, peer)
        };
        if !exchange.awaiting_review() {
            self.board.review = None;
            self.board.network.opened.insert(exchange.id.clone());
            return self.render_connection(bot, &exchange.connection_id, theme, window, cx);
        }
        if self
            .board
            .review
            .as_ref()
            .is_none_or(|(id, _)| id != exchange_id)
        {
            let draft = exchange.answer.clone().unwrap_or_default();
            let input = cx.new(|cx| {
                let mut input = TextInput::settings_prompt_field("The answer to send", cx);
                input.set_text(draft, cx);
                input
            });
            self.board.review = Some((exchange_id.to_string(), input));
        }
        let input = self.board.review.as_ref().map(|(_, input)| input.clone())?;
        let (who, theirs) = peer.as_ref().map_or_else(
            || ("A connected workspace".to_string(), None),
            super::network::peer_names,
        );
        let head = PanelHead {
            glyph: super::items::REVIEW_GLYPH,
            tint: Some(theme.warning),
            title: who.clone(),
        };

        // ── the bar: it waits on the owner; the two ways to decide ──
        let status = super::items::header_status(
            format!("bot-review-status-{exchange_id}"),
            icon(super::items::REVIEW_GLYPH)
                .size(px(13.0))
                .text_color(theme.warning)
                .into_any_element(),
            "Answer to review",
            theme.warning,
        );
        let (send_input, id_send, id_decline) =
            (input.clone(), exchange.id.clone(), exchange.id.clone());
        let original = exchange.answer.clone().unwrap_or_default();
        let (after_send, after_decline) = (
            PanelItem::Connection(exchange.connection_id.clone()),
            PanelItem::Connection(exchange.connection_id.clone()),
        );
        let controls = vec![
            popover::btn_ghost(theme, "Decline", "bot-review-decline")
                .id("bot-review-decline")
                .flex_none()
                .px(px(10.0))
                .py(px(4.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mutate(
                        serde_json::json!({
                            "op": "reviewBotExchange",
                            "exchangeId": id_decline,
                            "send": false,
                        }),
                        cx,
                    );
                    this.board.network.opened.insert(id_decline.clone());
                    this.show_panel(after_decline.clone(), cx);
                }))
                .into_any_element(),
            popover::btn_primary(theme, "Send")
                .id("bot-review-send")
                .flex_none()
                .py(px(4.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let text = send_input.read(cx).text().trim().to_string();
                    // Over its cap the engine refuses it; say so and stay here
                    // with the draft rather than leave for a failed send.
                    if let Err(refusal) = cypher_proto::check_review_text(&text) {
                        this.sidebar.notice = Some(refusal.into());
                        return cx.notify();
                    }
                    let edited = (text != original.trim()).then_some(text);
                    this.mutate(
                        serde_json::json!({
                            "op": "reviewBotExchange",
                            "exchangeId": id_send,
                            "send": true,
                            "text": edited,
                        }),
                        cx,
                    );
                    this.board.network.opened.insert(id_send.clone());
                    this.show_panel(after_send.clone(), cx);
                }))
                .into_any_element(),
        ];
        let asked = match ago(exchange.asked_at, now).as_str() {
            "now" => "asked just now".to_string(),
            span => format!("asked {span} ago"),
        };
        let line = match theirs {
            Some(theirs) => format!("{theirs} · {asked}"),
            None => asked,
        };
        self.board.panel_header = Some(super::items::item_header(
            status,
            controls,
            div()
                .mt(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .children(
                    peer.as_ref()
                        .map(|peer| super::network::peer_avatar(peer, 20.0, theme)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(16.0))
                        .line_height(px(22.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from(who)),
                ),
            Some(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(line))
                    .into_any_element(),
            ),
            theme,
        ));

        // ── what they asked, as a quote; then the draft, to edit ──
        let body = div()
            .p(px(8.0))
            .pb(px(14.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .mx(px(8.0))
                    .mt(px(6.0))
                    .mb(px(12.0))
                    .pl(px(10.0))
                    .border_l_2()
                    .border_color(crate::kit::theme::hairline(0.2))
                    .child(super::items::panel_markdown(
                        &exchange.question,
                        format!("bot-review-question-{exchange_id}"),
                        12.5,
                        theme.text_muted,
                        theme,
                        window,
                    )),
            )
            .child(
                div()
                    .mx(px(8.0))
                    .child(popover::dialog_field(input.into_any_element()).text_size(px(13.0))),
            );
        Some((head, body))
    }
}

/// What a card draws under its head: its title (a session's question; the
/// other kinds are titled by their item) and its body with the buttons.
struct NeedContent {
    title: String,
    body: gpui::Div,
}

impl NeedContent {
    fn of(body: gpui::Div) -> Self {
        Self {
            title: String::new(),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_fold_to_icons_only_as_far_as_the_line_needs() {
        use OptionRole::{Deny, Primary, Secondary, Stop};
        // As the extension lists them: the narrowest approval first.
        let roles = [Primary, Secondary, Secondary, Deny, Stop];
        let widths = [82.0, 143.0, 106.0, 55.0, 112.0];
        let fold = |room: f32| folded_options(&roles, &widths, room);
        // 498 of buttons and five gaps.
        assert_eq!(fold(528.0), [false; 5]);
        // The refusals go first, together.
        assert_eq!(fold(527.0), [false, false, false, true, true]);
        assert_eq!(fold(417.0), [false, false, false, true, true]);
        // Then the broadest approval, then the next one.
        assert_eq!(fold(416.0), [false, false, true, true, true]);
        assert_eq!(fold(339.0), [false, false, true, true, true]);
        assert_eq!(fold(338.0), [false, true, true, true, true]);
        // The narrowest approval keeps its label however little room.
        assert_eq!(fold(0.0), [false, true, true, true, true]);
        // An ordinary pair fits anywhere.
        assert_eq!(
            folded_options(&[Primary, Deny], &[90.0, 55.0], 288.0),
            [false; 2]
        );
    }
    use crate::test_fixtures;

    fn bot() -> Bot {
        Bot {
            id: "b".into(),
            name: "Atlas".into(),
            chat_id: "bot-chat".into(),
            ..test_fixtures::bot()
        }
    }

    #[test]
    fn a_question_reads_from_its_text_else_its_header() {
        let mut question = UserInputQuestion {
            id: "q".into(),
            header: "Table".into(),
            question: "Migrate invoices or credits?".into(),
            options: vec![],
            multi_select: false,
            permission: None,
            asked_by: None,
        };
        assert_eq!(question_text(&question), "Migrate invoices or credits?");
        question.question = "  ".into();
        assert_eq!(question_text(&question), "Table");
    }

    #[test]
    fn items_name_where_they_come_from() {
        let mut state = crate::state::AppState::new();
        let bot = bot();
        state.spaces = vec![cypher_proto::Space {
            id: "sp".into(),
            name: Some("web-api".into()),
            ..test_fixtures::space()
        }];
        let mut worker = cypher_proto::Chat {
            id: "s1".into(),
            title: Some("Migrate tables".into()),
            space_id: Some("sp".into()),
            ..test_fixtures::chat()
        };
        worker.started_by = Some(cypher_proto::ChatStartedBy {
            bot_id: "b".into(),
            work_id: Some("w".into()),
        });
        let mut routine_thread = cypher_proto::Chat {
            id: "t1".into(),
            title: Some("Morning brief".into()),
            ..test_fixtures::chat()
        };
        routine_thread.bot = Some(cypher_proto::BotChat::thread(
            "b",
            cypher_proto::BotOrigin::Routine,
            cypher_proto::BotThread::Routine {
                routine_id: "r".into(),
            },
        ));
        state.chats = vec![worker, routine_thread];
        let mut work = test_fixtures::bot_work("w", "b");
        work.title = "Fix invoice rounding".into();
        work.steps[0].sessions = vec!["s1".into()];
        let mut routine = test_fixtures::bot_routine("r", "b");
        routine.name = "Morning brief".into();
        state.bot_records.work = vec![work];
        state.bot_records.routines = vec![routine];
        let needs = vec![
            Need::Record(NeedsYou::Asking {
                chat_id: "s1".into(),
                work_id: Some("w".into()),
            }),
            Need::Record(NeedsYou::Asking {
                chat_id: "t1".into(),
                work_id: None,
            }),
            Need::Record(NeedsYou::Plan("w".into())),
        ];
        let items = need_items(&state, &bot, needs);
        let sources: Vec<&str> = items.iter().map(|i| i.source.as_str()).collect();
        assert_eq!(
            sources,
            [
                "Fix invoice rounding › Migrate tables",
                "Morning brief · routine",
                "Plan to approve",
            ]
        );
        let keys: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["ask-s1", "ask-t1", "plan-w"]);
    }
}
