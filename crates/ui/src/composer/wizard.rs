//! The question wizard: its pure reducer and the composer glue that drives it.

use super::*;

/// Reducer outcome of a wizard interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum WizardStep {
    Stay,
    /// Single-select landed — advance after [`AUTO_ADVANCE_MS`].
    AutoAdvance,
    /// All pages answered — submit these answers.
    Done(Vec<UserInputAnswer>),
}

/// Paged question state ("1/3"): single-select auto-advances, multi-select and
/// typed answers advance explicitly, number keys 1-9 select, Back pages back.
#[derive(Debug, Clone)]
pub struct Wizard {
    pub request_id: String,
    pub questions: Vec<UserInputQuestion>,
    pub page: usize,
    /// When set, this picker came from a slash-command extension (e.g.
    /// `/subagent-config`), not an in-turn agent question.
    pub slash: Option<SharedString>,
    /// Mounted right behind an answer (the follow-up stage of a two-stage
    /// question): the swap is instant, with no entrance fade. Decided once, at
    /// mount — see [`WIZARD_HANDOFF_QUIET_MS`].
    pub quiet_entry: bool,
    /// Per page: what the card shows beyond the wire question.
    views: Vec<PageView>,
    picked: Vec<Vec<usize>>,
    typed: Vec<String>,
}

/// Display-side reading of one page, recovered from the wire question.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageView {
    /// Per-option description (pi-ask-user's multi-select fallback carries
    /// them inline in its prompt).
    pub descriptions: Vec<Option<String>>,
    /// pi-ask-user's "type custom response" sentinel option: a way out of the
    /// list, not one of the answers, so the card styles it apart.
    pub custom_ix: Option<usize>,
    /// A text prompt listing options, turned into checkboxes: the answer goes
    /// back as the one comma-separated string that prompt asked for.
    pub joined: bool,
}

impl Wizard {
    pub fn new(request_id: String, questions: Vec<UserInputQuestion>) -> Self {
        let n = questions.len();
        let (questions, views) = questions.into_iter().map(read_page).unzip();
        Self {
            request_id,
            questions,
            page: 0,
            slash: None,
            quiet_entry: false,
            views,
            picked: vec![Vec::new(); n],
            typed: vec![String::new(); n],
        }
    }

    pub fn view(&self) -> PageView {
        self.views.get(self.page).cloned().unwrap_or_default()
    }

    pub fn for_slash(mut self, command: impl Into<SharedString>) -> Self {
        self.slash = Some(command.into());
        self
    }

    /// Mount without the entrance fade (this card follows an answer).
    pub fn quietly(mut self) -> Self {
        self.quiet_entry = true;
        self
    }

    pub fn current(&self) -> Option<&UserInputQuestion> {
        self.questions.get(self.page)
    }

    pub fn is_picked(&self, option_ix: usize) -> bool {
        self.picked
            .get(self.page)
            .is_some_and(|p| p.contains(&option_ix))
    }

    /// Whether the current page has any picked option.
    pub fn page_has_pick(&self) -> bool {
        self.picked.get(self.page).is_some_and(|p| !p.is_empty())
    }

    /// Click/tap an option.
    pub fn select(&mut self, option_ix: usize) -> WizardStep {
        let Some(question) = self.questions.get(self.page) else {
            return WizardStep::Stay;
        };
        if option_ix >= question.options.len() {
            return WizardStep::Stay;
        }
        let multi = question.multi_select;
        let Some(picked) = self.picked.get_mut(self.page) else {
            return WizardStep::Stay;
        };
        if multi {
            match picked.iter().position(|&p| p == option_ix) {
                Some(at) => {
                    picked.remove(at);
                }
                None => picked.push(option_ix),
            }
            WizardStep::Stay
        } else {
            *picked = vec![option_ix];
            WizardStep::AutoAdvance
        }
    }

    pub fn set_typed(&mut self, text: String) {
        if let Some(slot) = self.typed.get_mut(self.page) {
            *slot = text;
        }
    }

    /// Explicit submit / auto-advance landing.
    pub fn advance(&mut self) -> WizardStep {
        if self.page + 1 < self.questions.len() {
            self.page += 1;
            WizardStep::Stay
        } else {
            WizardStep::Done(self.answers())
        }
    }

    /// Page back; false when already on the first page.
    pub fn back(&mut self) -> bool {
        if self.page > 0 {
            self.page -= 1;
            true
        } else {
            false
        }
    }

    /// Answers per question: free text overrides picked labels.
    pub fn answers(&self) -> Vec<UserInputAnswer> {
        self.questions
            .iter()
            .enumerate()
            .map(|(ix, q)| {
                let typed = self.typed.get(ix).map(|s| s.trim()).unwrap_or("");
                let labels = if !typed.is_empty() {
                    vec![typed.to_string()]
                } else {
                    let labels: Vec<String> = self
                        .picked
                        .get(ix)
                        .map(|picked| {
                            picked
                                .iter()
                                .filter_map(|&p| q.options.get(p).cloned())
                                .collect()
                        })
                        .unwrap_or_default();
                    let joined = self.views.get(ix).is_some_and(|v| v.joined);
                    if joined && !labels.is_empty() {
                        vec![labels.join(", ")]
                    } else if labels.is_empty()
                        && optional_comment_copy(&q.header, &q.question).is_some()
                    {
                        // A blank optional comment is an answer, not a
                        // dismissal: no labels at all would reach pi-ask-user
                        // as a cancel and drop the option already picked.
                        vec![String::new()]
                    } else {
                        labels
                    }
                };
                UserInputAnswer {
                    question_id: q.id.clone(),
                    labels,
                }
            })
            .collect()
    }
}

/// Structured copy carried by pi-ask-user's RPC fallback for its second-stage
/// optional-comment editor:
///
/// ```text
/// question
///
/// Context:
/// context
///
/// Selected option:
/// - choice
/// ```
///
/// The TUI renders these as distinct visual sections. Parse that transport
/// string so the desktop app can do the same instead of showing one flat
/// paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OptionalCommentCopy {
    pub(super) question: String,
    pub(super) context: Option<String>,
    pub(super) selected_label: &'static str,
    pub(super) selected: String,
}

/// A page whose options ARE the whole answer: a single-select list. Such a page
/// does not render the shared free-text input, so whatever the composer draft
/// holds while it is up is not an answer to it.
///
/// Every other shape owns the input: a question with no options (free text —
/// pi-ask-user's freeform stage and its optional-comment stage both land here)
/// and multi-select, where typed text overrides the ticked boxes.
pub(super) fn wizard_pick_only(question: &UserInputQuestion) -> bool {
    !question.options.is_empty() && !question.multi_select
}

/// pi-ask-user's freeform sentinel (`FREEFORM_SENTINEL` in its index.ts).
pub(super) const ASK_USER_CUSTOM_OPTION: &str = "\u{270f}\u{fe0f} Type custom response...";

/// The heading pi-ask-user's RPC fallback puts over a multi-select option
/// list it asks the user to TYPE their picks against.
const ASK_USER_MULTI_OPTIONS: &str = "\n\nOptions (select one or more):\n";

/// Recover what the RPC dialog flattened: a multi-select prompt's option list
/// becomes real checkboxes (with their descriptions), and the freeform
/// sentinel is marked so the card can render it as a way out.
fn read_page(mut question: UserInputQuestion) -> (UserInputQuestion, PageView) {
    let mut view = PageView::default();
    if question.options.is_empty()
        && !question.multi_select
        && let Some((prompt, options)) = parse_listed_options(&question.question)
    {
        question.question = prompt;
        question.multi_select = true;
        let (titles, descriptions) = options.into_iter().unzip();
        question.options = titles;
        view.descriptions = descriptions;
        view.joined = true;
    }
    view.custom_ix = question.options.iter().position(|label| {
        label == ASK_USER_CUSTOM_OPTION || label.trim_start().starts_with('\u{270f}')
    });
    (question, view)
}

/// One listed option: its title and, when given, its description.
type ListedOption = (String, Option<String>);

/// Split `prompt\n\nOptions (select one or more):\n1. title — description`
/// into the prompt and its `(title, description)` rows. Lines that don't
/// start the next number continue the previous description.
pub(super) fn parse_listed_options(prompt: &str) -> Option<(String, Vec<ListedOption>)> {
    let prompt = prompt.replace("\r\n", "\n");
    let (before, list) = prompt.split_once(ASK_USER_MULTI_OPTIONS)?;
    let mut options: Vec<ListedOption> = Vec::new();
    for line in list.lines() {
        let next = format!("{}. ", options.len() + 1);
        if let Some(row) = line.strip_prefix(&next) {
            let option = match row.split_once(" — ") {
                Some((title, description)) => {
                    (title.trim().to_owned(), Some(description.trim().to_owned()))
                }
                None => (row.trim().to_owned(), None),
            };
            options.push(option);
        } else if let Some((_, description)) = options.last_mut() {
            let line = line.trim();
            if !line.is_empty() {
                let text = description.get_or_insert_with(String::new);
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(line);
            }
        } else if !line.trim().is_empty() {
            return None;
        }
    }
    if options.is_empty() || options.iter().any(|(title, _)| title.is_empty()) {
        return None;
    }
    Some((before.trim().to_owned(), options))
}

pub(super) fn wizard_context_block(
    context: &str,
    scope: crate::markdown::selection::SelectionScope,
    key: Arc<str>,
    theme: &crate::kit::theme::Theme,
) -> gpui::Div {
    div()
        .mt(px(10.0))
        .pl(px(10.0))
        .border_l_2()
        .border_color(crate::kit::theme::ink(0.12))
        .text_size(px(12.5))
        .line_height(px(18.0))
        .text_color(theme.text_muted)
        .cursor_text()
        .child(crate::markdown::render::selectable_plain_text(
            scope,
            key,
            SharedString::from(context.to_owned()),
            theme,
        ))
}

/// Section label inside the card ("Pick one", "Your answer", …).
pub(super) fn wizard_section_label(label: &str, theme: &crate::kit::theme::Theme) -> gpui::Div {
    div()
        .mb(px(8.0))
        .text_size(px(11.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_faint)
        .child(SharedString::from(label.to_owned()))
}

/// A keyboard hint in the card footer: a small keycap and what it does.
pub(super) fn wizard_key_hint(
    key: &str,
    action: &str,
    theme: &crate::kit::theme::Theme,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.0))
        .child(
            div()
                .h(px(18.0))
                .min_w(px(18.0))
                .px(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border)
                .bg(crate::kit::theme::ink(0.03))
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text_muted)
                .child(SharedString::from(key.to_owned())),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(action.to_owned())),
        )
}

pub(super) fn split_question_context(prompt: &str) -> (String, Option<String>) {
    let prompt = prompt.replace("\r\n", "\n");
    if let Some((question, context)) = prompt.split_once("\n\nContext:\n") {
        (
            question.trim().to_owned(),
            Some(context.trim().to_owned()).filter(|text| !text.is_empty()),
        )
    } else {
        (prompt.trim().to_owned(), None)
    }
}

pub(super) fn optional_comment_copy(header: &str, prompt: &str) -> Option<OptionalCommentCopy> {
    if header != "Optional comment" {
        return None;
    }
    let prompt = prompt.replace("\r\n", "\n");
    let (before_selected, selected_label, selected) =
        if let Some((before, selected)) = prompt.split_once("\n\nSelected option:\n") {
            (before, "Selected option", selected)
        } else if let Some((before, selected)) = prompt.split_once("\n\nSelected options:\n") {
            (before, "Selected options", selected)
        } else {
            return None;
        };
    let (question, context) = split_question_context(before_selected);
    let selected = selected
        .lines()
        .map(|line| line.trim().strip_prefix("- ").unwrap_or(line.trim()))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if question.trim().is_empty() || selected.is_empty() {
        return None;
    }
    Some(OptionalCommentCopy {
        question: question.trim().to_owned(),
        context: context.filter(|text| !text.is_empty()),
        selected_label,
        selected,
    })
}

impl Composer {
    pub(super) fn wizard_select(&mut self, option_ix: usize, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        let pick_only = wizard.current().is_some_and(wizard_pick_only);
        let last_page = wizard.page + 1 >= wizard.questions.len();
        let step = wizard.select(option_ix);
        if !pick_only {
            let has_pick = wizard.page_has_pick();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(
                    if has_pick {
                        "Type your own answer, or leave this blank to use the selected option"
                    } else {
                        "Type your own answer, or pick an option above"
                    },
                    cx,
                )
            });
        }
        match step {
            WizardStep::AutoAdvance if pick_only && last_page => self.wizard_advance(cx),
            WizardStep::AutoAdvance => self.schedule_auto_advance(cx),
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            WizardStep::Stay => {}
        }
        cx.notify();
    }

    fn schedule_auto_advance(&mut self, cx: &mut Context<Self>) {
        self.advance_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(AUTO_ADVANCE_MS))
                .await;
            this.update(cx, |composer, cx| composer.wizard_advance(cx))
                .ok();
        }));
    }

    /// Fold the shared free-text input into the current page before it is
    /// answered.
    ///
    /// Only pages that RENDER that input own it (see `pick_only` in
    /// [`Self::render_wizard`]): on a pick-only page the composer input is not
    /// part of the card at all and still holds the user's chat draft, which
    /// must never be mistaken for an answer.
    fn wizard_capture_typed(&mut self, cx: &mut Context<Self>) {
        let takes_typed = self
            .wizard
            .as_ref()
            .and_then(Wizard::current)
            .is_some_and(|q| !wizard_pick_only(q));
        if !takes_typed {
            return;
        }
        let typed = self.input.read(cx).text().trim().to_string();
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.set_typed(typed);
        }
    }

    pub(super) fn wizard_advance(&mut self, cx: &mut Context<Self>) {
        // Every route into an answer lands here — the panel's Submit/Next
        // button, Enter (in the input or on the card), and the auto-advance
        // timer — so the typed answer is collected HERE rather than at each
        // call site. A button that only checked the input was non-empty to
        // decide it could advance, then advanced without reading it, sent
        // empty labels: pi-ask-user's freeform stage reads that as cancelled
        // and throws the answer away.
        self.wizard_capture_typed(cx);
        // A late auto-advance timer can land after its card is gone (the
        // answer already went out); there is nothing left to advance.
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        match wizard.advance() {
            WizardStep::Done(answers) => self.wizard_finish(answers, cx),
            _ => {
                // Moving on: clear the shared free-text input for the next page.
                self.input.update(cx, |input, cx| input.set_text("", cx));
                cx.notify();
            }
        }
    }

    pub(super) fn wizard_back(&mut self, cx: &mut Context<Self>) {
        if let Some(wizard) = self.wizard.as_mut() {
            wizard.back();
            cx.notify();
        }
    }

    /// Skip pi-ask-user's optional comment: send the pick without one. This
    /// answers the stage (blank) — cancelling it would discard the pick.
    pub(super) fn wizard_skip_comment(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.wizard_advance(cx);
    }

    /// Dismiss a slash-command picker: empty answers map to Pi's
    /// `cancelled: true`, so the extension handler returns and the run ends.
    pub(super) fn wizard_cancel(&mut self, cx: &mut Context<Self>) {
        let Some(wizard) = self.wizard.as_ref() else {
            return;
        };
        let answers = wizard
            .questions
            .iter()
            .map(|q| UserInputAnswer {
                question_id: q.id.clone(),
                labels: Vec::new(),
            })
            .collect();
        self.wizard_finish(answers, cx);
    }

    /// Submit RespondInput and retire the card on the spot.
    ///
    /// Answering ENDS the card, always (user requirement). An answered card
    /// used to stay mounted but inert while the engine came back — which is
    /// how pi-ask-user's second stage (the optional comment, a separate input
    /// request one round trip behind the first) paged in without the panel
    /// unmounting. Inert is indistinguishable from frozen: the click landed on
    /// a card that then just sat there greyed out. So the panel goes now, the
    /// composer comes straight back, and a follow-up stage arrives as its own
    /// card — swapped in, not faded in, so the pair doesn't read as a flicker
    /// (see [`WIZARD_HANDOFF_QUIET_MS`]).
    fn wizard_finish(&mut self, answers: Vec<UserInputAnswer>, cx: &mut Context<Self>) {
        let Some(request_id) = self
            .wizard
            .as_ref()
            .map(|wizard| wizard.request_id.clone())
            // Already on its way: a late click, key or auto-advance timer must
            // never answer the same request twice.
            .filter(|request_id| !self.answered_requests.contains(request_id))
        else {
            return;
        };
        // The card leaves either way; only the send below needs an engine.
        self.wizard = None;
        self.advance_task = None;
        let (engine, chat_id) = {
            let state = self.state.read(cx);
            match (state.engine().cloned(), state.selected_chat.clone()) {
                (Some(engine), Some(chat_id)) => (engine, chat_id),
                _ => {
                    self.input
                        .update(cx, |input, cx| input.set_placeholder("Do anything…", cx));
                    cx.notify();
                    return;
                }
            }
        };
        self.answered_requests.insert(request_id.clone());
        self.answered_at = Some(Instant::now());
        self.input_swap_instant = true;
        self.input.update(cx, |input, cx| {
            input.set_text("", cx);
            // The panel borrowed the composer input; hand back its identity.
            input.set_placeholder("Do anything…", cx);
        });
        // RPC transport branch: main answers through `QueueCommand`
        // `RespondInput`; a temporary side chat through `RespondSideChatInput`.
        let transport = self.transport.clone();
        let params = match &transport {
            ComposerTransport::Main => {
                let command = SessionCommandPayload::RespondInput {
                    request_id: request_id.clone(),
                    answers,
                };
                match serde_json::to_value(&command) {
                    Ok(value) => serde_json::json!({ "chatId": chat_id, "command": value }),
                    Err(_) => return,
                }
            }
            ComposerTransport::SideChat(side) => {
                let mut params = serde_json::Map::new();
                params.insert(
                    "sideChatId".into(),
                    serde_json::Value::String(side.side_chat_id.clone()),
                );
                params.insert(
                    "requestId".into(),
                    serde_json::Value::String(request_id.clone()),
                );
                params.insert(
                    "answers".into(),
                    serde_json::to_value(&answers).unwrap_or_default(),
                );
                side.with_target(&mut params, self.state.read(cx).local_device_id.as_deref());
                serde_json::Value::Object(params)
            }
        };
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    if matches!(transport, ComposerTransport::SideChat(_)) {
                        methods::RESPOND_SIDE_CHAT_INPUT
                    } else {
                        methods::QUEUE_COMMAND
                    },
                    params,
                )
                .await;
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.failure = Some(format!("Answer failed: {err}").into());
                    // The answer never left this device — put the panel back.
                    composer.answered_requests.remove(&request_id);
                    cx.notify();
                })
                .ok();
                return;
            }
            // Safety net against a dead-looking session: the command queued,
            // but the host may still REJECT it (e.g. the run's resolver is
            // gone). If the very same request is still the live pending input
            // once the host has had ample time to execute and the resolved
            // flag to sync back, the answer demonstrably didn't take — bring
            // the panel back instead of leaving the question unanswerable.
            //
            // [`WIZARD_STUCK_ANSWER_MS`], not a couple of seconds: this must
            // never race a slow-but-healthy round trip, or the card the user
            // just answered reappears for a moment before the resolved flag
            // lands — the flash this whole path is trying to avoid.
            cx.background_executor()
                .timer(Duration::from_millis(WIZARD_STUCK_ANSWER_MS))
                .await;
            this.update(cx, |composer, cx| {
                let transcript = composer.state.read(cx).transcript.clone();
                let still_pending = pending_input_request(&transcript)
                    .is_some_and(|(pending_id, _)| pending_id == request_id);
                // The panel lifecycle rebuilds the card from the pending
                // request as soon as it stops being suppressed.
                if still_pending && composer.answered_requests.remove(&request_id) {
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn on_wizard_key(
        &mut self,
        event: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // Keys bubbling out of the free-text input must not double-handle:
        // digits select options only while the input is empty, and Enter is the
        // input's own Submit action when it has focus.
        let input_focused = self.input.read(cx).focus_handle.is_focused(window);
        let input_empty = self.input.read(cx).is_empty();
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if key == "c" && (modifiers.platform || modifiers.control) {
            // Clicking card text focuses the card, not the input, so Copy
            // has no input binding to reach: copy the text selection here.
            if let Some(text) = crate::markdown::selection::selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
            }
        } else if let Ok(digit) = key.parse::<usize>()
            && (1..=9).contains(&digit)
        {
            if !input_focused || input_empty {
                self.wizard_select(digit - 1, cx);
                // Consumed as a selection: stop the platform from also
                // inserting the digit into the focused free-text input.
                cx.stop_propagation();
            }
        } else if key == "enter" {
            if !input_focused {
                if self.wizard_ready(cx) {
                    self.wizard_advance(cx);
                }
                cx.stop_propagation();
            }
        } else if key == "escape" && (!input_focused || input_empty) {
            let cancel = self
                .wizard
                .as_ref()
                .is_some_and(|w| w.page == 0 && w.current().is_some_and(wizard_pick_only));
            if cancel {
                self.wizard_cancel(cx);
            } else {
                self.wizard_back(cx);
            }
            cx.stop_propagation();
        }
    }
}
