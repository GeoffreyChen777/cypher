//! The **Needs you** card (§13.1): what waits on the owner, most pressing
//! first, right under Status in the amber the sidebar gives a session
//! awaiting input. Its rows are the bot's own question, actions and plans
//! awaiting approval, sessions the bot started that are asking, and review
//! drafts: decisions only, each listed here and nowhere else in the sidebar
//! (a failed routine is state, marked on its routine). The header opens the
//! whole list.
//!
//! Each item opens in the detail panel with its context and its actions:
//! the plan to approve, the action and why it needs approval, the worker's
//! question with its options and recent messages, the draft answer to send.
//! A decision is one owner field on the record (a `Mutate`), so every count
//! follows the record wherever it was made.

use super::board::{card_corner, card_glyph, card_item, two_lines};
use super::items::{
    ago, decision_bar, glyph, item_row, mark_cell, outcome_value, project_label, section_label,
    text_value, trailing,
};
use super::panel::{PanelHead, PanelItem};
use super::*;
use cypher_proto::{ApprovalState, ExchangeState, NeedsYou};

/// Rows the card shows before it points at the full list.
const CARD_ROWS: usize = 5;

/// One Needs you item as a row draws it.
struct NeedRow {
    /// What a click opens; `None` for the bot's own question, which lives in
    /// the chat beside the board.
    item: Option<PanelItem>,
    glyph: &'static str,
    tint: gpui::Hsla,
    title: String,
    sub: String,
    at: Option<chrono::DateTime<Utc>>,
}

fn need_rows(
    state: &crate::state::AppState,
    bot: &Bot,
    needs: Vec<Need>,
    theme: &Theme,
) -> Vec<NeedRow> {
    let records = &state.bot_records;
    let chat_title = |id: &str| {
        state
            .chats
            .iter()
            .find(|c| c.id == id)
            .map(|c| transcript::single_line(c.title.as_deref().unwrap_or("New session")))
    };
    needs
        .into_iter()
        .filter_map(|need| match need {
            Need::Chat => {
                let chat = state.chats.iter().find(|c| c.id == bot.chat_id)?;
                Some(NeedRow {
                    item: None,
                    glyph: icons::CHAT_ROUND_LINE,
                    tint: theme.warning,
                    title: format!("{} asked you something", transcript::single_line(&bot.name)),
                    sub: chat
                        .last_message_preview
                        .as_deref()
                        .map(transcript::single_line)
                        .unwrap_or_else(|| "In your conversation".into()),
                    at: chat.last_message_at,
                })
            }
            Need::Record(NeedsYou::Plan(id)) => {
                let work = records.work.iter().find(|w| w.id == id)?;
                let steps = work.steps.len();
                Some(NeedRow {
                    item: Some(PanelItem::Plan(id)),
                    glyph: icons::CHECKLIST,
                    tint: theme.warning,
                    title: transcript::single_line(&work.title),
                    sub: format!(
                        "Plan to approve · {steps} step{}",
                        if steps == 1 { "" } else { "s" }
                    ),
                    at: Some(super::items::at(work.created_at)),
                })
            }
            Need::Record(NeedsYou::Approval(id)) => {
                let approval = records.approvals.iter().find(|a| a.id == id)?;
                Some(NeedRow {
                    item: Some(PanelItem::Approval(id)),
                    glyph: icons::SHIELD_CHECK,
                    tint: theme.warning,
                    title: transcript::single_line(&approval.action),
                    sub: transcript::single_line(&approval.reason),
                    at: Some(super::items::at(approval.created_at)),
                })
            }
            Need::Record(NeedsYou::Asking { chat_id, work_id }) => {
                let chat = state.chats.iter().find(|c| c.id == chat_id)?;
                let project = chat
                    .space_id
                    .as_deref()
                    .and_then(|id| state.space_row(id))
                    .map(|s| s.display_name().to_string());
                let sub = match project {
                    Some(project) => format!("Asking · in {project}"),
                    None => "Asking".to_string(),
                };
                Some(NeedRow {
                    item: Some(PanelItem::Asking {
                        chat_id: chat_id.clone(),
                        work_id,
                    }),
                    glyph: icons::QUESTION_CIRCLE,
                    tint: theme.warning,
                    title: chat_title(&chat_id).unwrap_or_default(),
                    sub,
                    at: chat.last_message_at,
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
                Some(NeedRow {
                    item: Some(PanelItem::Review(id)),
                    glyph: icons::USERS,
                    tint: theme.warning,
                    title: transcript::single_line(&exchange.question),
                    sub: format!("Review the answer to {peer}"),
                    at: Some(super::items::at(exchange.asked_at)),
                })
            }
        })
        .collect()
}

impl Shell {
    fn current_need_rows(&self, bot: &Bot, theme: &Theme, cx: &App) -> Vec<NeedRow> {
        let state = self.state.read(cx);
        need_rows(state, bot, bot_needs(state, bot, Utc::now()), theme)
    }

    /// A Needs you row: its item opens in the panel; the bot's own question
    /// hands focus to the chat beside the board.
    fn need_row(
        &self,
        key: String,
        row: NeedRow,
        roomy: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now();
        let selected = row.item.is_some() && self.board.panel == row.item;
        let item = row.item.clone();
        let open = cx.listener(move |this, _, _, cx| match item.clone() {
            Some(item) => this.open_panel(item, cx),
            None => {
                this.close_panel(cx);
                this.tiles.focus_pending = true;
                cx.notify();
            }
        });
        if !roomy {
            // On the card: one line, what it is; the panel says what to do.
            return card_item(
                &key,
                Some(card_glyph(row.glyph, row.tint)),
                row.title,
                row.at
                    .map(|at| card_corner(format_time_ago(at, now), None, theme)),
                selected,
                theme,
            )
            .on_click(open);
        }
        item_row(&key, selected)
            .py(px(8.0))
            .on_click(open)
            .child(mark_cell(glyph(row.glyph, row.tint)))
            .child(two_lines(row.title, row.sub, String::new(), theme))
            .children(
                row.at
                    .map(|at| trailing(format_time_ago(at, now), theme.text_faint)),
            )
    }

    /// The sidebar card; `None` when there is nothing to show.
    pub(super) fn render_needs_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let rows = self.current_need_rows(bot, theme, cx);
        if rows.is_empty() {
            return None;
        }
        let total = rows.len();
        let header = self.card_header(
            "needs",
            "bot-needs-header",
            "Needs you",
            total,
            PanelItem::NeedsYou,
            Some(theme.warning),
            theme,
            cx,
        );
        let shown: Vec<_> = rows
            .into_iter()
            .take(CARD_ROWS)
            .enumerate()
            .map(|(ix, row)| self.need_row(format!("bot-need-{ix}"), row, false, theme, cx))
            .collect();
        let more = (total > CARD_ROWS).then(|| {
            card_item(
                "bot-needs-more",
                None,
                format!("{} more", total - CARD_ROWS),
                None,
                false,
                theme,
            )
            .text_color(theme.text_muted)
            .on_click(cx.listener(|this, _, _, cx| this.open_panel(PanelItem::NeedsYou, cx)))
        });
        Some(self.fold_card("needs", header, shown.into_iter().chain(more), theme))
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_needs_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        match item {
            PanelItem::NeedsYou => Some(self.render_needs_list(bot, theme, cx)),
            // A plan is a work item waiting on its approval: one page.
            PanelItem::Plan(id) => self.render_work_item(id, theme, window, cx),
            PanelItem::Approval(id) => self.render_approval_panel(bot, id, theme, cx),
            PanelItem::Asking { chat_id, work_id } => {
                self.render_asking_panel(bot, chat_id, work_id, theme, cx)
            }
            PanelItem::Review(id) => self.render_review_panel(id, theme, cx),
            _ => None,
        }
    }

    fn render_needs_list(
        &mut self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let rows = self.current_need_rows(bot, theme, cx);
        let head = PanelHead {
            glyph: icons::BELL,
            tint: Some(theme.warning),
            title: "Needs you".into(),
        };
        let body = div().p(px(8.0)).flex().flex_col().gap(px(2.0));
        let body = if rows.is_empty() {
            self.board.panel_empty = Some(
                parts::panel_empty(
                    theme,
                    icons::CHECK,
                    "You're all caught up",
                    "Plans, approvals, questions and answers to review land here.",
                )
                .into_any_element(),
            );
            body
        } else {
            body.children(rows.into_iter().enumerate().map(|(ix, row)| {
                self.need_row(format!("bot-panel-need-{ix}"), row, true, theme, cx)
            }))
        };
        (head, body)
    }

    /// An action a background session waits on: what it is, why it needs
    /// you and where it comes from; Decline / Approve in the footer.
    fn render_approval_panel(
        &mut self,
        bot: &Bot,
        approval_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let now = Utc::now();
        let (approval, work_title) = {
            let state = self.state.read(cx);
            let approval = state
                .bot_records
                .approvals
                .iter()
                .find(|a| a.id == approval_id)?
                .clone();
            let work_title = approval.work_id.as_deref().and_then(|id| {
                state
                    .bot_records
                    .work
                    .iter()
                    .find(|w| w.id == id)
                    .map(|w| w.title.clone())
            });
            (approval, work_title)
        };
        let awaiting = approval.awaiting();
        let head = PanelHead {
            glyph: icons::SHIELD_CHECK,
            tint: awaiting.then_some(theme.warning),
            title: if awaiting {
                "Approve action".into()
            } else {
                "Action".into()
            },
        };
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(parts::fact(theme, "Action", text_value(&approval.action)))
            .child(parts::fact(theme, "Why", text_value(&approval.reason)))
            .child(parts::fact(
                theme,
                "From",
                SharedString::from(format!(
                    "{} · {}",
                    work_title.unwrap_or_else(|| bot.name.clone()),
                    ago(approval.created_at, now)
                )),
            ));
        if awaiting {
            let (yes, no) = (approval.id.clone(), approval.id.clone());
            self.set_panel_footer(
                decision_bar(None, theme)
                    .child(
                        popover::btn_ghost(theme, "Decline", "bot-approval-decline")
                            .id("bot-approval-decline")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mutate(
                                    serde_json::json!({
                                        "op": "resolveBotApproval", "approvalId": no, "approve": false,
                                    }),
                                    cx,
                                )
                            })),
                    )
                    .child(
                        popover::btn_primary(theme, "Approve")
                            .id("bot-approval-approve")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mutate(
                                    serde_json::json!({
                                        "op": "resolveBotApproval", "approvalId": yes, "approve": true,
                                    }),
                                    cx,
                                )
                            })),
                    ),
            );
        } else {
            let decided = approval.decision.as_ref().map(|d| d.approve);
            let (text, ok) = match (approval.state, decided) {
                (ApprovalState::Approved, _) | (ApprovalState::Pending, Some(true)) => {
                    ("Approved", true)
                }
                (ApprovalState::Declined, _) | (ApprovalState::Pending, Some(false)) => {
                    ("Declined", false)
                }
                (ApprovalState::Expired, _) | (ApprovalState::Pending, None) => {
                    ("Expired before an answer", false)
                }
            };
            body = body.child(parts::fact(theme, "Status", outcome_value(text, ok, theme)));
        }
        Some((head, body))
    }

    /// A session the bot started, asking: its question with the options as
    /// answers, its latest messages, and the way into the session.
    fn render_asking_panel(
        &mut self,
        bot: &Bot,
        chat_id: &str,
        work_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let (title, place, work_title, status) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|c| c.id == chat_id)?;
            let place = chat
                .space_id
                .as_deref()
                .and_then(|id| project_label(state, id, theme));
            let work_title = state
                .bot_records
                .work
                .iter()
                .find(|w| w.id == work_id)
                .map(|w| w.title.clone());
            (
                transcript::single_line(chat.title.as_deref().unwrap_or("New session")),
                place,
                work_title,
                state.display_status_for(chat, Utc::now()),
            )
        };
        let peek = self.peek(chat_id, false, cx);
        let (pending, recent, loaded) = {
            let state = peek.read(cx);
            (
                crate::composer::pending_input_request(&state.transcript),
                recent_messages(&state.transcript, 3),
                !state.transcript.is_empty(),
            )
        };
        let head = PanelHead {
            glyph: icons::QUESTION_CIRCLE,
            tint: (status == ChatIndicator::AwaitingInput).then_some(theme.warning),
            title,
        };
        let open = chat_id.to_string();
        self.set_panel_actions(vec![
            super::items::icon_button(
                "bot-asking-open",
                icons::CHAT_ROUND_LINE,
                "Open the session",
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open_chat(open.clone(), cx)))
            .into_any_element(),
        ]);
        let mut body = div().p(px(8.0)).flex().flex_col();
        if let Some(place) = &place {
            body = body.child(parts::fact(
                theme,
                "Project",
                SharedString::from(format!("{} · {}", place.name, place.device)),
            ));
        }
        body = body.child(parts::fact(
            theme,
            "From",
            SharedString::from(work_title.unwrap_or_else(|| bot.name.clone())),
        ));
        match pending {
            Some((request_id, questions)) if !self.board.answered.contains(&request_id) => {
                let first = questions.first().cloned();
                if let Some(question) = &first {
                    let (headline, context) = crate::composer::split_question_context(
                        if question.question.trim().is_empty() {
                            &question.header
                        } else {
                            &question.question
                        },
                    );
                    let question = match context {
                        Some(context) => format!("{headline}\n\n{context}"),
                        None => headline,
                    };
                    body = body.child(parts::fact(theme, "Question", text_value(&question)));
                }
                let simple = questions.len() == 1
                    && first
                        .as_ref()
                        .is_some_and(|q| !q.multi_select && !q.options.is_empty());
                match first.filter(|_| simple) {
                    Some(question) => {
                        body = body.child(section_label("Answer", theme)).children(
                            question.options.iter().enumerate().map(|(ix, option)| {
                                let key = format!("bot-asking-option-{ix}");
                                let (chat, request, question_id, label) = (
                                    chat_id.to_string(),
                                    request_id.clone(),
                                    question.id.clone(),
                                    option.clone(),
                                );
                                // Resting like the start screen's suggestion
                                // cards, so each reads as an answer to pick.
                                item_row(&key, false)
                                    .mx(px(8.0))
                                    .mb(px(4.0))
                                    .py(px(8.0))
                                    .bg(motion::hover_blend(
                                        &key,
                                        crate::kit::theme::ink(0.035),
                                        crate::kit::theme::ink(0.07),
                                    ))
                                    .child(mark_cell(
                                        div()
                                            .size(px(16.0))
                                            .rounded(px(5.0))
                                            .bg(crate::kit::theme::ink(0.06))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(10.0))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(theme.text_muted)
                                            .child(SharedString::from((ix + 1).to_string())),
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(px(13.0))
                                            .line_height(px(18.0))
                                            .text_color(theme.text)
                                            .child(SharedString::from(option.clone())),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.answer_session(
                                            chat.clone(),
                                            request.clone(),
                                            question_id.clone(),
                                            label.clone(),
                                            cx,
                                        )
                                    }))
                            }),
                        );
                    }
                    None => {
                        body = body.child(parts::fact(
                            theme,
                            "Answer",
                            SharedString::from("In the session"),
                        ));
                    }
                }
            }
            Some(_) => {
                body = body.child(parts::fact(
                    theme,
                    "Status",
                    outcome_value("Answer sent", true, theme),
                ));
            }
            None if loaded => {
                body = body.child(parts::fact(
                    theme,
                    "Status",
                    outcome_value("Nothing to answer right now", true, theme),
                ));
            }
            None => {
                body = body.child(parts::fact(
                    theme,
                    "Status",
                    SharedString::from("Loading the session…"),
                ));
            }
        }
        if !recent.is_empty() {
            body = body.child(section_label("Latest in the session", theme));
            body = body.children(recent.into_iter().map(|(user, text)| {
                div()
                    .px(px(8.0))
                    .py(px(5.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted.opacity(0.8))
                            .child(SharedString::from(if user {
                                bot.name.clone()
                            } else {
                                "Session".to_string()
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .line_height(px(18.0))
                            .text_color(theme.text.opacity(0.85))
                            .child(SharedString::from(text)),
                    )
            }));
        }
        Some((head, body))
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
        self.board.answered.insert(request_id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let params = serde_json::json!({ "chatId": chat_id, "command": command });
            if let Err(err) = engine.client().call(methods::QUEUE_COMMAND, params).await {
                this.update(cx, |this, cx| {
                    this.board.answered.remove(&request_id);
                    this.sidebar.notice = Some(format!("Answer failed: {err}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// A peer's question and the answer the bot drafted, editable, then
    /// Send / Decline.
    fn render_review_panel(
        &mut self,
        exchange_id: &str,
        theme: &Theme,
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
                .map(|c| {
                    if c.peer.owner_name.is_empty() {
                        c.peer.name.clone()
                    } else {
                        format!("{} ({})", c.peer.name, c.peer.owner_name)
                    }
                })
                .unwrap_or_else(|| "A connected workspace".into());
            (exchange, peer)
        };
        let awaiting = exchange.awaiting_review();
        if awaiting
            && self
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
        let head = PanelHead {
            glyph: icons::USERS,
            tint: awaiting.then_some(theme.warning),
            title: "Review answer".into(),
        };
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(parts::fact(
                theme,
                "From",
                SharedString::from(format!("{peer} · {}", ago(exchange.asked_at, now))),
            ))
            .child(parts::fact(
                theme,
                "Question",
                text_value(&exchange.question),
            ));
        let input = self
            .board
            .review
            .as_ref()
            .filter(|(id, _)| awaiting && id == exchange_id)
            .map(|(_, input)| input.clone());
        match input {
            Some(input) => {
                let (send_input, id_send, id_decline) =
                    (input.clone(), exchange.id.clone(), exchange.id.clone());
                let original = exchange.answer.clone().unwrap_or_default();
                body =
                    body.child(parts::field_label(theme, "Answer"))
                        .child(div().mx(px(8.0)).child(
                            popover::dialog_field(input.into_any_element()).text_size(px(13.0)),
                        ));
                self.set_panel_footer(
                    decision_bar(Some("Edit it before it goes, or decline."), theme)
                        .child(
                            popover::btn_ghost(theme, "Decline", "bot-review-decline")
                                .id("bot-review-decline")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mutate(
                                        serde_json::json!({
                                            "op": "reviewBotExchange",
                                            "exchangeId": id_decline,
                                            "send": false,
                                        }),
                                        cx,
                                    )
                                })),
                        )
                        .child(
                            popover::btn_primary(theme, "Send")
                                .id("bot-review-send")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let text = send_input.read(cx).text().trim().to_string();
                                    let edited = (text != original.trim()).then_some(text);
                                    this.mutate(
                                        serde_json::json!({
                                            "op": "reviewBotExchange",
                                            "exchangeId": id_send,
                                            "send": true,
                                            "text": edited,
                                        }),
                                        cx,
                                    )
                                })),
                        ),
                );
            }
            None => {
                let sent = exchange.review.as_ref().map(|r| (r.send, r.text.clone()));
                let shown = sent
                    .as_ref()
                    .and_then(|(_, text)| text.clone())
                    .or_else(|| exchange.answer.clone());
                if let Some(answer) = shown.filter(|a| !a.trim().is_empty()) {
                    body = body.child(parts::fact(theme, "Answer", text_value(&answer)));
                }
                let (text, ok) = match (exchange.state, sent) {
                    (ExchangeState::Answered, _) | (_, Some((true, _))) => ("Sent", true),
                    (ExchangeState::Declined, _) | (_, Some((false, _))) => ("Declined", false),
                    (ExchangeState::Expired, _) => ("Expired", false),
                    _ => ("Waiting on its answer", false),
                };
                body = body.child(parts::fact(theme, "Status", outcome_value(text, ok, theme)));
            }
        }
        Some((head, body))
    }
}

/// The last `n` messages of a session with text, oldest first, as (sent by
/// the bot, text): one paragraph each, clipped.
fn recent_messages(entries: &[cypher_doc::SessionMessageEntry], n: usize) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = entries
        .iter()
        .rev()
        .filter_map(|entry| {
            let user = match entry.role {
                cypher_doc::MessageRole::User => true,
                cypher_doc::MessageRole::Assistant => false,
                _ => return None,
            };
            let text: String = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    cypher_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let text = plain_preview(&text);
            (!text.is_empty()).then(|| (user, cypher_proto::clip(&text, 280)))
        })
        .take(n)
        .collect();
    out.reverse();
    out
}

/// Markdown read as one plain line for a preview: headings, list and quote
/// markers, emphasis and code ticks dropped, fenced code left out.
fn plain_preview(text: &str) -> String {
    let mut fenced = false;
    let mut words: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let line = line.trim_start_matches(['#', '>']).trim_start();
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| {
                let (number, rest) = line.split_once(". ")?;
                number.chars().all(|c| c.is_ascii_digit()).then_some(rest)
            })
            .unwrap_or(line);
        words.extend(line.split_whitespace());
    }
    words
        .join(" ")
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_read_markdown_as_plain_text() {
        assert_eq!(
            plain_preview(
                "## Streaming pipeline\n\n1. **Doc command** queues a `run` entry\n- Fold\n```rust\nlet x = 1;\n```\nDone."
            ),
            "Streaming pipeline Doc command queues a run entry Fold Done."
        );
    }

    #[test]
    fn recent_messages_keep_the_text_of_the_last_few() {
        let entry = |id: &str, role, text: &str| cypher_doc::SessionMessageEntry {
            id: id.into(),
            role,
            parts: vec![cypher_doc::MessagePart::Text {
                id: "t".into(),
                text: text.into(),
                agent_text: None,
            }],
            ..crate::test_fixtures::entry()
        };
        let entries = vec![
            entry("1", cypher_doc::MessageRole::User, "first"),
            entry("2", cypher_doc::MessageRole::Assistant, "  second\n\nline "),
            entry("3", cypher_doc::MessageRole::Assistant, ""),
            entry("4", cypher_doc::MessageRole::User, "third"),
        ];
        assert_eq!(
            recent_messages(&entries, 2),
            vec![
                (false, "second line".to_string()),
                (true, "third".to_string())
            ]
        );
    }
}
