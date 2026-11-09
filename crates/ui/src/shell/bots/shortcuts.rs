//! **Shortcuts**: buttons pinned at the foot of a workspace's sidebar, each
//! replaying a workflow the owner does often, its steps in order. A step
//! asks the workspace something in its chat, starts a session in one of its
//! projects, runs a routine, or opens a project, a session or a web page.
//! Each step starts once the one before it has been sent off; nothing waits
//! for an answer. A message may hold `{input}`: a click then asks for one
//! line of text first, in a field under the chips.
//!
//! The chips wrap under a quiet head whose + opens the form in the detail
//! panel; right-click a chip to edit, move or delete it. Shortcuts are
//! registry rows, synced like routines; only the runs and the form live
//! here.

use super::board::{CARD_TITLE_INSET, ROW_MARGIN, ROW_MARK_INSET, header_frame};
use super::panel::{PanelHead, PanelItem};
use super::parts::{self, CallError};
use super::*;
use cypher_proto::{
    BotShortcut, Chat, OpenTarget, SHORTCUT_INPUT, SHORTCUT_STEPS_MAX, ShortcutStep,
};

/// The icons a shortcut can wear, by the name it stores.
const SHORTCUT_ICONS: &[(&str, &str)] = &[
    ("bolt", icons::BOLT),
    ("star", icons::STAR),
    ("rocket", icons::ROCKET),
    ("play", icons::PLAY),
    ("chat", icons::CHAT_ROUND_LINE),
    ("checklist", icons::CHECKLIST),
    ("code", icons::CODE),
    ("terminal", icons::TERMINAL),
    ("bug", icons::BUG),
    ("test-tube", icons::TEST_TUBE),
    ("document", icons::DOCUMENT),
    ("global", icons::GLOBAL),
    ("calendar", icons::CALENDAR),
    ("cup-hot", icons::CUP_HOT),
    ("magic-stick", icons::MAGIC_STICK),
    ("flag", icons::FLAG),
];

/// How long a finished run's check stays on its chip.
const DONE_MS: u64 = 1600;
/// How long an armed Delete waits for its confirming click.
const DELETE_ARM_MS: u64 = 4000;
/// Sessions an Open step offers, newest first.
const RECENT_SESSIONS: usize = 12;

// ── steps ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepKind {
    Ask,
    Session,
    Routine,
    Open,
    /// A kind from a newer app: kept as it is, and skipped.
    Unknown,
}

/// What an Open step opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenKind {
    Project,
    Session,
    Url,
}

/// What a step's picker chooses from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Picks {
    Project,
    Routine,
    Session,
}

fn kind_of(step: &ShortcutStep) -> (StepKind, OpenKind) {
    match step {
        ShortcutStep::Ask { .. } => (StepKind::Ask, OpenKind::Project),
        ShortcutStep::Session { .. } => (StepKind::Session, OpenKind::Project),
        ShortcutStep::Routine { .. } => (StepKind::Routine, OpenKind::Project),
        ShortcutStep::Open { target } => (
            StepKind::Open,
            match target {
                OpenTarget::Project { .. } => OpenKind::Project,
                OpenTarget::Session { .. } => OpenKind::Session,
                OpenTarget::Url { .. } => OpenKind::Url,
            },
        ),
        ShortcutStep::Unknown => (StepKind::Unknown, OpenKind::Project),
    }
}

/// A step's name, as the form and a failed run call it.
pub(super) fn kind_title(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Ask => "Ask the workspace",
        StepKind::Session => "Start a session",
        StepKind::Routine => "Run a routine",
        StepKind::Open => "Open",
        StepKind::Unknown => "A newer step",
    }
}

fn kind_glyph(kind: StepKind, open: OpenKind) -> &'static str {
    match (kind, open) {
        (StepKind::Ask, _) => icons::CHAT_ROUND_LINE,
        (StepKind::Session, _) => icons::CODE,
        (StepKind::Routine, _) => icons::REFRESH,
        (StepKind::Open, OpenKind::Project) => icons::FOLDER,
        (StepKind::Open, OpenKind::Session) => icons::CHAT_ROUND_LINE,
        (StepKind::Open, OpenKind::Url) => icons::GLOBAL,
        (StepKind::Unknown, _) => icons::BOLT,
    }
}

/// A shortcut's icon: the one it picked, else its first step's.
pub(super) fn shortcut_glyph(shortcut: &BotShortcut) -> &'static str {
    shortcut
        .icon
        .as_deref()
        .and_then(icon_asset)
        .unwrap_or_else(|| match shortcut.steps.first() {
            Some(step) => {
                let (kind, open) = kind_of(step);
                kind_glyph(kind, open)
            }
            None => icons::BOLT,
        })
}

fn icon_asset(name: &str) -> Option<&'static str> {
    SHORTCUT_ICONS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, asset)| *asset)
}

/// `step` with the click's input in every blank.
pub(super) fn fill_step(step: &ShortcutStep, input: &str) -> ShortcutStep {
    let fill = |text: &str| text.replace(SHORTCUT_INPUT, input.trim());
    match step {
        ShortcutStep::Ask { text } => ShortcutStep::Ask { text: fill(text) },
        ShortcutStep::Session { space_id, text } => ShortcutStep::Session {
            space_id: space_id.clone(),
            text: fill(text),
        },
        other => other.clone(),
    }
}

/// A step in a few words: "Ask “What's stuck?”", "New session in Web",
/// "Run Morning brief", "Open ci.example.com". `name` names a project,
/// routine or session by its id.
pub(super) fn step_summary(step: &ShortcutStep, name: impl Fn(&str) -> Option<String>) -> String {
    let named = |id: &str, fallback: &str| name(id).unwrap_or_else(|| fallback.to_string());
    match step {
        ShortcutStep::Ask { text } => format!("Ask \u{201C}{}\u{201D}", clip(text, 40)),
        ShortcutStep::Session { space_id, .. } => {
            format!("New session in {}", named(space_id, "a project"))
        }
        ShortcutStep::Routine { routine_id } => format!("Run {}", named(routine_id, "a routine")),
        ShortcutStep::Open { target } => match target {
            OpenTarget::Project { space_id } => format!("Open {}", named(space_id, "a project")),
            OpenTarget::Session { chat_id } => format!("Open {}", named(chat_id, "a session")),
            OpenTarget::Url { url } => format!("Open {}", url_host(url)),
        },
        ShortcutStep::Unknown => "A step from a newer version".into(),
    }
}

/// One line of `text`, at most `max` characters.
fn clip(text: &str, max: usize) -> String {
    let line = transcript::single_line(text);
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// "https://ci.example.com/runs" → "ci.example.com".
fn url_host(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// What a run says when a step fails: "Morning stopped at step 2, Run a
/// routine: That routine was deleted."
pub(super) fn failure_line(label: &str, step: usize, kind: StepKind, why: &str) -> String {
    format!(
        "{label} stopped at step {step}, {}: {why}",
        kind_title(kind).to_lowercase()
    )
}

/// The ids after moving `id` one place by `delta` (-1 earlier, +1 later);
/// `None` when it is already at that end.
pub(super) fn moved(ids: &[String], id: &str, delta: isize) -> Option<Vec<String>> {
    let from = ids.iter().position(|i| i == id)?;
    let to = from
        .checked_add_signed(delta)
        .filter(|to| *to < ids.len())?;
    let mut ids = ids.to_vec();
    ids.swap(from, to);
    Some(ids)
}

/// Where a new shortcut goes: after the workspace's others.
pub(super) fn next_order(shortcuts: &[&BotShortcut]) -> i64 {
    shortcuts.iter().map(|s| s.order).max().map_or(0, |o| o + 1)
}

// ── the form, as data ───────────────────────────────────────────────────────

/// A step's choices besides its text fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StepPick {
    pub kind: StepKind,
    pub open: OpenKind,
    /// The project a session starts in, or the one an Open step opens.
    pub space_id: Option<String>,
    pub routine_id: Option<String>,
    pub chat_id: Option<String>,
}

impl StepPick {
    pub(super) fn new(kind: StepKind) -> Self {
        Self {
            kind,
            open: OpenKind::Project,
            space_id: None,
            routine_id: None,
            chat_id: None,
        }
    }

    /// What its picker chooses from; `None` for a step without one.
    pub(super) fn picks(&self) -> Option<Picks> {
        match (self.kind, self.open) {
            (StepKind::Session, _) | (StepKind::Open, OpenKind::Project) => Some(Picks::Project),
            (StepKind::Routine, _) => Some(Picks::Routine),
            (StepKind::Open, OpenKind::Session) => Some(Picks::Session),
            _ => None,
        }
    }

    /// The id its picker holds.
    pub(super) fn picked(&self) -> Option<&str> {
        match self.picks()? {
            Picks::Project => self.space_id.as_deref(),
            Picks::Routine => self.routine_id.as_deref(),
            Picks::Session => self.chat_id.as_deref(),
        }
    }

    pub(super) fn pick(&mut self, id: String) {
        match self.picks() {
            Some(Picks::Project) => self.space_id = Some(id),
            Some(Picks::Routine) => self.routine_id = Some(id),
            Some(Picks::Session) => self.chat_id = Some(id),
            None => {}
        }
    }

    /// Whether it has a message to write.
    fn has_text(&self) -> bool {
        matches!(self.kind, StepKind::Ask | StepKind::Session)
    }

    fn glyph(&self) -> &'static str {
        kind_glyph(self.kind, self.open)
    }
}

/// One step of the form as typed: its picks, its message and its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StepDraft {
    pub pick: StepPick,
    pub text: String,
    pub url: String,
}

impl StepDraft {
    pub(super) fn from_step(step: &ShortcutStep) -> Self {
        let (kind, open) = kind_of(step);
        let mut pick = StepPick {
            open,
            ..StepPick::new(kind)
        };
        let (mut text, mut url) = (String::new(), String::new());
        match step {
            ShortcutStep::Ask { text: t } => text = t.clone(),
            ShortcutStep::Session { space_id, text: t } => {
                pick.space_id = Some(space_id.clone());
                text = t.clone();
            }
            ShortcutStep::Routine { routine_id } => pick.routine_id = Some(routine_id.clone()),
            ShortcutStep::Open { target } => match target {
                OpenTarget::Project { space_id } => pick.space_id = Some(space_id.clone()),
                OpenTarget::Session { chat_id } => pick.chat_id = Some(chat_id.clone()),
                OpenTarget::Url { url: u } => url = u.clone(),
            },
            ShortcutStep::Unknown => {}
        }
        Self { pick, text, url }
    }

    /// The step it describes, or what its picker still needs ("a
    /// project").
    pub(super) fn build(&self) -> Result<ShortcutStep, &'static str> {
        let picked = || {
            self.pick
                .picked()
                .map(str::to_string)
                .ok_or(match self.pick.picks() {
                    Some(Picks::Routine) => "a routine",
                    Some(Picks::Session) => "a session",
                    _ => "a project",
                })
        };
        let text = self.text.trim().to_string();
        Ok(match self.pick.kind {
            StepKind::Ask => ShortcutStep::Ask { text },
            StepKind::Session => ShortcutStep::Session {
                space_id: picked()?,
                text,
            },
            StepKind::Routine => ShortcutStep::Routine {
                routine_id: picked()?,
            },
            StepKind::Open => ShortcutStep::Open {
                target: match self.pick.open {
                    OpenKind::Project => OpenTarget::Project {
                        space_id: picked()?,
                    },
                    OpenKind::Session => OpenTarget::Session { chat_id: picked()? },
                    OpenKind::Url => OpenTarget::Url {
                        url: self.url.trim().to_string(),
                    },
                },
            },
            StepKind::Unknown => ShortcutStep::Unknown,
        })
    }
}

/// The shortcut a form describes, checked. `existing` keeps its id, order
/// and creation time; a new one takes `order`.
pub(super) fn build_shortcut(
    label: &str,
    icon: Option<String>,
    steps: &[StepDraft],
    existing: Option<&BotShortcut>,
    bot_id: &str,
    order: i64,
    now: i64,
) -> Result<BotShortcut, String> {
    let steps = steps
        .iter()
        .enumerate()
        .map(|(ix, step)| {
            step.build()
                .map_err(|what| format!("Step {} needs {what}.", ix + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut shortcut = BotShortcut {
        id: existing.map_or_else(|| uuid::Uuid::new_v4().to_string(), |s| s.id.clone()),
        bot_id: bot_id.to_string(),
        label: label.trim().to_string(),
        icon,
        steps,
        order: existing.map_or(order, |s| s.order),
        created_at: existing.map_or(now, |s| s.created_at),
        updated_at: now,
    };
    shortcut.clamp();
    shortcut.validate()?;
    Ok(shortcut)
}

// ── panel state ─────────────────────────────────────────────────────────────

/// A shortcut's latest run, as its chip shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RunState {
    Running,
    Done,
    Failed(String),
}

/// The shortcut waiting on its input, and the field it is typed in.
struct Asking {
    id: String,
    input: Entity<TextInput>,
    _submit: Subscription,
}

/// The form's open menu: Add step, or a step's picker (its index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormMenu {
    AddStep,
    Pick(usize),
}

struct StepForm {
    pick: StepPick,
    text: Entity<TextInput>,
    url: Entity<TextInput>,
}

impl StepForm {
    fn draft(&self, cx: &App) -> StepDraft {
        StepDraft {
            pick: self.pick.clone(),
            text: self.text.read(cx).text().to_string(),
            url: self.url.read(cx).text().to_string(),
        }
    }
}

struct ShortcutForm {
    /// The shortcut it edits; `None` for a new one.
    id: Option<String>,
    bot_id: String,
    name: Entity<TextInput>,
    icon: Option<String>,
    steps: Vec<StepForm>,
    menu: Option<FormMenu>,
    error: Option<SharedString>,
    busy: bool,
    _submit: Subscription,
}

impl ShortcutForm {
    fn item(&self) -> PanelItem {
        match &self.id {
            Some(id) => PanelItem::Shortcut(id.clone()),
            None => PanelItem::NewShortcut,
        }
    }
}

/// The shortcuts' own state: runs by shortcut id (with the run that set
/// them, so a stale reset leaves a newer run alone), the field asking for
/// an input, the chip menu with its armed Delete, and the form.
#[derive(Default)]
pub(super) struct ShortcutsUi {
    runs: std::collections::HashMap<String, (u64, RunState)>,
    last_run: u64,
    asking: Option<Asking>,
    /// The chip menu: (shortcut id, window position).
    menu: popover::Popup<(String, Point<Pixels>)>,
    delete_armed: Option<String>,
    delete_disarm: Option<Task<()>>,
    form: Option<ShortcutForm>,
}

/// One choice in a step's picker.
struct Choice {
    id: String,
    glyph: &'static str,
    tint: gpui::Hsla,
    label: String,
    note: String,
}

impl Shell {
    fn shortcut_row(&self, id: &str, cx: &App) -> Option<BotShortcut> {
        self.state
            .read(cx)
            .bot_records
            .shortcuts
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// The current workspace and its shortcuts, in order.
    fn board_shortcuts(&self, cx: &App) -> Option<(Bot, Vec<BotShortcut>)> {
        let state = self.state.read(cx);
        let bot = current_bot(&state.bots, self.settings.botboard_bot.as_deref())?.clone();
        let shortcuts = state
            .bot_records
            .shortcuts_of(&bot.id)
            .into_iter()
            .cloned()
            .collect();
        Some((bot, shortcuts))
    }

    /// Projects, routines and sessions by id to their names, for step
    /// summaries.
    fn step_names(&self, cx: &App) -> std::collections::HashMap<String, String> {
        let state = self.state.read(cx);
        let mut names: std::collections::HashMap<String, String> = state
            .spaces
            .iter()
            .map(|s| (s.id.clone(), s.display_name().to_string()))
            .collect();
        names.extend(
            state
                .bot_records
                .routines
                .iter()
                .map(|r| (r.id.clone(), r.name.clone())),
        );
        names.extend(state.chats.iter().map(|c| {
            (
                c.id.clone(),
                transcript::single_line(c.title.as_deref().unwrap_or("New session")),
            )
        }));
        names
    }

    // ── the area ────────────────────────────────────────────────────────────

    /// The shortcuts pinned under the workspace's cards: a quiet head with
    /// its +, then the chips (or a one-line hint), the field a chip asks
    /// for its input in, and what a failed run said. `None` with no
    /// workspace.
    pub(in crate::shell) fn render_board_shortcuts(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (_, shortcuts) = self.board_shortcuts(cx)?;
        if self
            .board
            .shortcuts
            .asking
            .as_ref()
            .is_some_and(|asking| !shortcuts.iter().any(|s| s.id == asking.id))
        {
            self.board.shortcuts.asking = None;
        }
        let add_key = "bot-shortcuts-add";
        let add = div()
            .id(add_key)
            .flex_none()
            .size(px(22.0))
            .mr(px(-5.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(motion::hover_blend(
                add_key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener(add_key))
            .tooltip(|_, cx| {
                cx.new(|_| super::super::session::FindTooltip("New shortcut".into()))
                    .into()
            })
            .on_click(
                cx.listener(|this, _, window, cx| this.open_shortcut_form(None, Some(window), cx)),
            )
            .child(
                icon(icons::PLUS)
                    .size(px(11.0))
                    .text_color(motion::hover_blend(
                        add_key,
                        theme.text_muted.opacity(0.8),
                        theme.text,
                    )),
            );
        let header = header_frame(
            "bot-shortcuts-header",
            "Shortcuts",
            self.card_focused("shortcuts", cx),
            theme,
        )
        .pr(px(7.0))
        .child(div().flex_1())
        .child(add);
        let mut card = super::board::card(theme).child(header);
        if shortcuts.is_empty() {
            card = card.child(
                div()
                    .pl(px(CARD_TITLE_INSET))
                    .pr(px(10.0))
                    .pb(px(GROUP_CARD_BODY_PADDING + 6.0))
                    .mt(px(-4.0))
                    .text_size(px(11.5))
                    .line_height(px(16.0))
                    .text_color(theme.text_muted.opacity(0.6))
                    .child(SharedString::from("Add buttons for things you do often")),
            );
        } else {
            let names = self.step_names(cx);
            let names = |id: &str| names.get(id).cloned();
            let open = self.board.panel.clone();
            let chips: Vec<_> = shortcuts
                .iter()
                .map(|shortcut| {
                    let selected = open.as_ref() == Some(&PanelItem::Shortcut(shortcut.id.clone()));
                    self.shortcut_chip(shortcut, &names, selected, theme, cx)
                })
                .collect();
            let failures: Vec<_> = shortcuts
                .iter()
                .filter_map(
                    |shortcut| match self.board.shortcuts.runs.get(&shortcut.id) {
                        Some((_, RunState::Failed(line))) => {
                            Some((shortcut.id.clone(), line.clone()))
                        }
                        _ => None,
                    },
                )
                .map(|(id, line)| {
                    div()
                        .id(SharedString::from(format!("bot-shortcut-failed-{id}")))
                        .mx(px(ROW_MARGIN))
                        .px(px(ROW_MARK_INSET))
                        .pt(px(4.0))
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(theme.danger.opacity(0.9))
                        .cursor_pointer()
                        .tooltip(|_, cx| {
                            cx.new(|_| super::super::session::FindTooltip("Dismiss".into()))
                                .into()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.board.shortcuts.runs.remove(&id);
                            cx.notify();
                        }))
                        .child(SharedString::from(line))
                })
                .collect();
            card = card.child(
                div()
                    .pb(px(GROUP_CARD_BODY_PADDING + 4.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .mx(px(ROW_MARGIN))
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(4.0))
                            .children(chips),
                    )
                    .children(self.render_shortcut_input(theme, cx))
                    .children(failures),
            );
        }
        Some(
            div()
                .flex_none()
                .px(px(Theme::SPACE_SM))
                .pb(px(Theme::SPACE_SM))
                .child(
                    div()
                        .border_t_1()
                        .border_color(crate::kit::theme::hairline(0.06))
                        .child(card),
                )
                .into_any_element(),
        )
    }

    /// One chip: its icon (a spinner while it runs, a check once done, a
    /// warning when a step failed) and label, a pill that lifts on hover.
    /// A click runs it; a right-click opens its menu.
    fn shortcut_chip(
        &self,
        shortcut: &BotShortcut,
        names: &impl Fn(&str) -> Option<String>,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let key = format!("bot-shortcut-{}", shortcut.id);
        let run = self
            .board
            .shortcuts
            .runs
            .get(&shortcut.id)
            .map(|(_, run)| run);
        let ticker = self.pulse_ticker.entity_id();
        let mark = match run {
            Some(RunState::Running) => {
                super::super::render::status_mark(ChatIndicator::Working, ticker, theme)
            }
            Some(RunState::Done) => {
                super::super::render::status_mark(ChatIndicator::Completed, ticker, theme)
            }
            Some(RunState::Failed(_)) => Some(
                icon(icons::DANGER_TRIANGLE)
                    .size(px(12.0))
                    .text_color(theme.danger)
                    .into_any_element(),
            ),
            None => None,
        }
        .unwrap_or_else(|| {
            icon(shortcut_glyph(shortcut))
                .size(px(12.0))
                .text_color(theme.text_muted)
                .into_any_element()
        });
        let tip: SharedString = match run {
            Some(RunState::Failed(line)) => line.clone(),
            _ => shortcut
                .steps
                .iter()
                .map(|step| step_summary(step, names))
                .collect::<Vec<_>>()
                .join(" \u{2192} "),
        }
        .into();
        let hover = crate::appearance::surface_style::sidebar_hover(theme);
        let rest = if selected {
            crate::appearance::surface_style::sidebar_selected(theme)
        } else {
            crate::kit::theme::ink(0.045)
        };
        let (menu_id, run_id) = (shortcut.id.clone(), shortcut.id.clone());
        div()
            .id(SharedString::from(key.clone()))
            .flex_none()
            .max_w_full()
            .h(px(26.0))
            .pl(px(ROW_MARK_INSET))
            .pr(px(10.0))
            .rounded(px(13.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .bg(motion::hover_blend(&key, rest, hover))
            .active(|s| s.bg(crate::kit::theme::wash(0.14)))
            .text_size(px(12.0))
            .text_color(motion::hover_blend(
                &key,
                theme.text.opacity(if selected { 1.0 } else { 0.85 }),
                theme.text,
            ))
            .on_hover(motion::hover_listener(key))
            .tooltip(move |_, cx| {
                cx.new(|_| super::super::session::FindTooltip(tip.clone()))
                    .into()
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.click_shortcut(&run_id, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.board.shortcuts.delete_armed = None;
                    this.board
                        .shortcuts
                        .menu
                        .open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_none()
                    .size(px(14.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mark),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(shortcut.label.clone())),
            )
    }

    /// The field a chip asks for its input in: Enter runs, Esc cancels.
    fn render_shortcut_input(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let asking = self.board.shortcuts.asking.as_ref()?;
        Some(
            div()
                .mx(px(ROW_MARGIN))
                .mt(px(6.0))
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(crate::kit::theme::hairline(0.08))
                .bg(crate::kit::theme::ink(0.04))
                .text_size(px(12.5))
                .text_color(theme.text)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    let keys = &event.keystroke;
                    if keys.key == "escape" && !keys.modifiers.modified() {
                        this.board.shortcuts.asking = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(asking.input.clone().into_any_element())
                .into_any_element(),
        )
    }

    // ── running ─────────────────────────────────────────────────────────────

    /// A chip's click: ask for its input first when a step wants one, else
    /// run it. A chip already running ignores it.
    fn click_shortcut(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shortcut) = self.shortcut_row(id, cx) else {
            return;
        };
        if matches!(
            self.board.shortcuts.runs.get(id),
            Some((_, RunState::Running))
        ) {
            return;
        }
        if !shortcut.wants_input() {
            return self.run_shortcut(shortcut, String::new(), window, cx);
        }
        if let Some(asking) = self.board.shortcuts.asking.as_ref()
            && asking.id == id
        {
            window.focus(&asking.input.focus_handle(cx), cx);
            return;
        }
        let input = cx.new(|cx| {
            TextInput::settings_field(
                format!(
                    "Text for {}, then Enter",
                    transcript::single_line(&shortcut.label)
                ),
                false,
                cx,
            )
        });
        let shortcut_id = shortcut.id.clone();
        let submit = cx.subscribe_in(
            &input,
            window,
            move |this: &mut Self, input, event: &TextInputEvent, window, cx| {
                if !matches!(event, TextInputEvent::Submitted) {
                    return;
                }
                let text = input.read(cx).text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                this.board.shortcuts.asking = None;
                if let Some(shortcut) = this.shortcut_row(&shortcut_id, cx) {
                    this.run_shortcut(shortcut, text, window, cx);
                }
                cx.notify();
            },
        );
        window.focus(&input.focus_handle(cx), cx);
        self.board.shortcuts.asking = Some(Asking {
            id: shortcut.id,
            input,
            _submit: submit,
        });
        cx.notify();
    }

    /// Run `shortcut`'s steps in order with `input` in their blanks. Each
    /// starts once the one before it has gone out (a routine's once the
    /// host took the request); the first that fails stops the rest and
    /// says why on the chip.
    fn run_shortcut(
        &mut self,
        shortcut: BotShortcut,
        input: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ui = &mut self.board.shortcuts;
        ui.last_run += 1;
        let run = ui.last_run;
        ui.runs
            .insert(shortcut.id.clone(), (run, RunState::Running));
        let steps: Vec<ShortcutStep> = shortcut
            .steps
            .iter()
            .map(|step| fill_step(step, &input))
            .collect();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut outcome = RunState::Done;
            for (ix, step) in steps.iter().enumerate() {
                let Ok(started) = this.update_in(cx, |this, window, cx| {
                    this.start_step(&shortcut.bot_id, step, window, cx)
                }) else {
                    return;
                };
                let result = match started {
                    Ok(Some(params)) => {
                        let engine = this
                            .update(cx, |this, cx| this.state.read(cx).engine().cloned())
                            .ok()
                            .flatten();
                        match engine {
                            Some(engine) => engine
                                .client()
                                .call(methods::MUTATE, params)
                                .await
                                .map(|_| ())
                                .map_err(|err| CallError::from(err).to_string()),
                            None => Err("Engine not connected".to_string()),
                        }
                    }
                    Ok(None) => Ok(()),
                    Err(why) => Err(why),
                };
                if let Err(why) = result {
                    outcome = RunState::Failed(failure_line(
                        &shortcut.label,
                        ix + 1,
                        kind_of(step).0,
                        &why,
                    ));
                    break;
                }
            }
            let done = outcome == RunState::Done;
            let set = this.update(cx, |this, cx| {
                if let Some(entry) = this.board.shortcuts.runs.get_mut(&shortcut.id)
                    && entry.0 == run
                {
                    entry.1 = outcome;
                }
                cx.notify();
            });
            if set.is_err() || !done {
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(DONE_MS))
                .await;
            this.update(cx, |this, cx| {
                let runs = &mut this.board.shortcuts.runs;
                if runs.get(&shortcut.id).is_some_and(|(at, _)| *at == run) {
                    runs.remove(&shortcut.id);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Start one step for `bot_id`'s shortcut. A step that is a call
    /// returns its Mutate params for the run to send; a reference gone
    /// stale fails with what went.
    fn start_step(
        &mut self,
        bot_id: &str,
        step: &ShortcutStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<serde_json::Value>, String> {
        let bot = self
            .bot_row(bot_id, cx)
            .ok_or_else(|| "The workspace was deleted.".to_string())?;
        let project_seen = |this: &Self, space_id: &str, cx: &App| {
            bot.scope.sees_space(space_id) && this.state.read(cx).space_row(space_id).is_some()
        };
        let no_project = || "That project is gone, or the workspace no longer sees it.".to_string();
        match step {
            ShortcutStep::Unknown => {}
            ShortcutStep::Ask { text } => {
                // Its chat in front, on the Botboard.
                self.open_bot(bot_id, cx);
                let composer = self
                    .board
                    .slot
                    .filter(|_| self.board_chat() == Some(bot.chat_id.as_str()))
                    .and_then(|sid| self.tiles.slots.get(&sid))
                    .map(|slot| slot.composer.clone())
                    .ok_or_else(|| "The workspace's chat isn't ready yet.".to_string())?;
                composer.update(cx, |composer, cx| composer.send_aside(text, cx));
                self.tiles.focus_pending = true;
            }
            ShortcutStep::Session { space_id, text } => {
                if !project_seen(self, space_id, cx) {
                    return Err(no_project());
                }
                if !self.start_session_with(space_id.clone(), text, window, cx) {
                    return Err("A new session couldn't open.".into());
                }
            }
            ShortcutStep::Routine { routine_id } => {
                let exists = self
                    .state
                    .read(cx)
                    .bot_records
                    .routines
                    .iter()
                    .any(|r| r.id == *routine_id && r.bot_id == bot_id);
                if !exists {
                    return Err("That routine was deleted.".into());
                }
                return Ok(Some(
                    serde_json::json!({ "op": "runBotRoutine", "routineId": routine_id }),
                ));
            }
            ShortcutStep::Open { target } => match target {
                OpenTarget::Project { space_id } => {
                    if !project_seen(self, space_id, cx) {
                        return Err(no_project());
                    }
                    self.open_bot(bot_id, cx);
                    self.show_panel(PanelItem::Project(space_id.clone()), cx);
                }
                OpenTarget::Session { chat_id } => {
                    let live = self
                        .state
                        .read(cx)
                        .chats
                        .iter()
                        .any(|c| c.id == *chat_id && !c.archived);
                    if !live {
                        return Err("That session was deleted or archived.".into());
                    }
                    self.open_chat(chat_id.clone(), cx);
                }
                OpenTarget::Url { url } => cx.open_url(url),
            },
        }
        Ok(None)
    }

    // ── the chip menu ───────────────────────────────────────────────────────

    fn close_shortcut_menu(&mut self, cx: &mut Context<Self>) {
        self.board.shortcuts.delete_armed = None;
        self.board.shortcuts.delete_disarm = None;
        if self.board.shortcuts.menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.board.shortcuts.menu);
            cx.notify();
        }
    }

    /// A chip's menu: Edit, Move left, Move right, and Delete, which asks
    /// once more by turning red until a second click within a moment.
    pub(super) fn render_shortcut_menu(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (id, position) = self.board.shortcuts.menu.get().cloned()?;
        let closing = self.board.shortcuts.menu.closing_since();
        let (bot, shortcuts) = self.board_shortcuts(cx)?;
        let ids: Vec<String> = shortcuts.iter().map(|s| s.id.clone()).collect();
        let row = |key: &str, glyph: &'static str, label: &str, enabled: bool, danger: bool| {
            let (ink, mark) = if danger {
                (theme.danger, theme.danger)
            } else {
                (theme.text, theme.text_muted)
            };
            popover::menu_row(theme, false, format!("bot-shortcut-menu-{key}"))
                .id(SharedString::from(format!("bot-shortcut-menu-{key}")))
                .when(!enabled, |el| el.opacity(0.4).cursor_default())
                .text_color(ink)
                .child(icon(glyph).size(px(16.0)).text_color(mark))
                .child(SharedString::from(label.to_string()))
        };
        let edit_id = id.clone();
        let edit = row("edit", icons::PEN, "Edit", true, false).on_click(cx.listener(
            move |this, _, window, cx| {
                this.close_shortcut_menu(cx);
                this.open_shortcut_form(Some(edit_id.clone()), Some(window), cx);
            },
        ));
        let mover =
            |key: &str, glyph: &'static str, label: &str, delta: isize, cx: &mut Context<Self>| {
                let order = moved(&ids, &id, delta);
                let bot_id = bot.id.clone();
                row(key, glyph, label, order.is_some(), false).when_some(order, |el, order| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        this.close_shortcut_menu(cx);
                        this.board_mutate(
                            serde_json::json!({
                                "op": "orderBotShortcuts", "botId": bot_id, "shortcutIds": order,
                            }),
                            cx,
                        );
                    }))
                })
            };
        let left = mover("left", icons::ARROW_LEFT, "Move left", -1, cx);
        let right = mover("right", icons::ARROW_RIGHT, "Move right", 1, cx);
        let armed = self.board.shortcuts.delete_armed.as_deref() == Some(id.as_str());
        let delete_id = id.clone();
        let delete = row(
            "delete",
            icons::TRASH_BIN_MINIMALISTIC,
            if armed {
                "Click again to delete"
            } else {
                "Delete"
            },
            true,
            true,
        )
        .when(armed, |el| el.bg(theme.danger.opacity(0.14)))
        .on_click(
            cx.listener(move |this, _, _, cx| this.arm_or_delete_shortcut(delete_id.clone(), cx)),
        );
        let menu = popover::popover_card(theme)
            .w(px(170.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_shortcut_menu(cx)))
            .flex()
            .flex_col()
            .child(edit)
            .child(left)
            .child(right)
            .child(popover::menu_separator())
            .child(delete)
            .into_any_element();
        Some(popover::menu_at(
            "bot-shortcut-menu",
            position,
            menu,
            closing,
        ))
    }

    fn arm_or_delete_shortcut(&mut self, id: String, cx: &mut Context<Self>) {
        if self.board.shortcuts.delete_armed.as_deref() == Some(id.as_str()) {
            self.close_shortcut_menu(cx);
            self.board.shortcuts.runs.remove(&id);
            if self.board.panel == Some(PanelItem::Shortcut(id.clone())) {
                self.board.shortcuts.form = None;
                self.close_panel(cx);
            }
            self.board_mutate(
                serde_json::json!({ "op": "deleteBotShortcut", "shortcutId": id }),
                cx,
            );
            return cx.notify();
        }
        self.board.shortcuts.delete_armed = Some(id.clone());
        self.board.shortcuts.delete_disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(DELETE_ARM_MS))
                .await;
            this.update(cx, |this, cx| {
                if this.board.shortcuts.delete_armed.as_deref() == Some(id.as_str()) {
                    this.board.shortcuts.delete_armed = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    // ── the form ────────────────────────────────────────────────────────────

    /// Open the form for shortcut `id` (`None` for a new one) in the panel.
    /// A new shortcut's draft stays while it is unsaved; an edit starts
    /// from the record.
    fn open_shortcut_form(
        &mut self,
        id: Option<String>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let item = id
            .clone()
            .map_or(PanelItem::NewShortcut, PanelItem::Shortcut);
        if self.board.shortcuts.form.as_ref().map(ShortcutForm::item) != Some(item.clone()) {
            let Some(form) = self.new_shortcut_form(id, cx) else {
                return;
            };
            self.board.shortcuts.form = Some(form);
        }
        if let (Some(window), Some(form)) = (window, self.board.shortcuts.form.as_ref()) {
            window.focus(&form.name.focus_handle(cx), cx);
        }
        self.show_panel(item, cx);
        cx.notify();
    }

    /// A form for shortcut `id`, or a new one starting with an Ask step.
    /// `None` when the shortcut is gone.
    fn new_shortcut_form(
        &mut self,
        id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<ShortcutForm> {
        let (bot, shortcuts) = self.board_shortcuts(cx)?;
        let existing = match &id {
            Some(id) => Some(shortcuts.into_iter().find(|s| s.id == *id)?),
            None => None,
        };
        let name = cx.new(|cx| TextInput::settings_field("Morning check-in", false, cx));
        let drafts: Vec<StepDraft> = match &existing {
            Some(shortcut) => {
                name.update(cx, |input, cx| input.set_text(shortcut.label.clone(), cx));
                shortcut.steps.iter().map(StepDraft::from_step).collect()
            }
            None => vec![StepDraft {
                pick: StepPick::new(StepKind::Ask),
                text: String::new(),
                url: String::new(),
            }],
        };
        let steps = drafts
            .into_iter()
            .map(|draft| new_step_form(draft, cx))
            .collect();
        let submit = cx.subscribe(&name, |this: &mut Self, _, event: &TextInputEvent, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.save_shortcut_form(cx);
            }
        });
        Some(ShortcutForm {
            id,
            bot_id: bot.id,
            name,
            icon: existing.and_then(|s| s.icon),
            steps,
            menu: None,
            error: None,
            busy: false,
            _submit: submit,
        })
    }

    fn close_shortcut_form(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self.board.shortcuts.form.take()
            && self.board.panel == Some(form.item())
        {
            self.close_panel(cx);
        }
        cx.notify();
    }

    fn save_shortcut_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.board.shortcuts.form.as_ref() else {
            return;
        };
        if form.busy {
            return;
        }
        let label = form.name.read(cx).text().to_string();
        let drafts: Vec<StepDraft> = form.steps.iter().map(|step| step.draft(cx)).collect();
        let built = {
            let records = &self.state.read(cx).bot_records;
            let mine = records.shortcuts_of(&form.bot_id);
            let existing = form
                .id
                .as_ref()
                .and_then(|id| mine.iter().copied().find(|s| s.id == *id));
            build_shortcut(
                &label,
                form.icon.clone(),
                &drafts,
                existing,
                &form.bot_id,
                next_order(&mine),
                cypher_proto::now_ms(),
            )
        };
        let form = self.board.shortcuts.form.as_mut().expect("checked above");
        let shortcut = match built {
            Ok(shortcut) => shortcut,
            Err(message) => {
                form.error = Some(message.into());
                return cx.notify();
            }
        };
        form.busy = true;
        form.error = None;
        let item = form.item();
        cx.notify();
        self.board_call(
            methods::MUTATE,
            serde_json::json!({ "op": "saveBotShortcut", "shortcut": shortcut }),
            cx,
            move |this, result, cx| match result {
                Ok(_) => {
                    this.board.shortcuts.form = None;
                    if this.board.panel.as_ref() == Some(&item) {
                        this.close_panel(cx);
                    }
                }
                Err(err) => {
                    if let Some(form) = this.board.shortcuts.form.as_mut() {
                        form.busy = false;
                        form.error = Some(
                            match err {
                                CallError::Unknown => {
                                    "This workspace's device can't save shortcuts yet.".to_string()
                                }
                                CallError::Failed(text) => text,
                            }
                            .into(),
                        );
                    }
                }
            },
        );
    }

    fn with_shortcut_form(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut ShortcutForm)) {
        if let Some(form) = self.board.shortcuts.form.as_mut() {
            edit(form);
            form.error = None;
        }
        cx.notify();
    }

    /// What a step's picker offers: the workspace's projects, its routines,
    /// or its recent sessions.
    fn step_choices(&self, bot: &Bot, picks: Picks, theme: &Theme, cx: &App) -> Vec<Choice> {
        match picks {
            Picks::Project => self
                .routine_projects(bot, cx)
                .into_iter()
                .map(|(space, device)| Choice {
                    glyph: crate::appearance::space_style::space_icon(space.icon.as_deref()),
                    tint: crate::appearance::space_style::space_color(
                        space.color.as_deref(),
                        theme,
                    )
                    .unwrap_or(theme.text_muted),
                    label: space.display_name().to_string(),
                    note: device,
                    id: space.id,
                })
                .collect(),
            Picks::Routine => self
                .state
                .read(cx)
                .bot_records
                .routines
                .iter()
                .filter(|r| r.bot_id == bot.id)
                .map(|r| Choice {
                    id: r.id.clone(),
                    glyph: super::schedule::routine_brief(r).0,
                    tint: theme.text_muted,
                    label: r.name.clone(),
                    note: String::new(),
                })
                .collect(),
            Picks::Session => {
                let state = self.state.read(cx);
                let mut chats: Vec<&Chat> = bot
                    .scope
                    .visible_chats(&state.chats)
                    .into_iter()
                    .filter(|c| {
                        !c.archived && !c.is_child() && !c.is_memory_pass() && c.bot.is_none()
                    })
                    .collect();
                chats.sort_by_key(|c| std::cmp::Reverse(c.last_message_at.unwrap_or(c.created_at)));
                chats
                    .into_iter()
                    .take(RECENT_SESSIONS)
                    .map(|c| Choice {
                        id: c.id.clone(),
                        glyph: icons::CHAT_ROUND_LINE,
                        tint: theme.text_muted,
                        label: transcript::single_line(c.title.as_deref().unwrap_or("New session")),
                        note: c
                            .space_id
                            .as_deref()
                            .and_then(|id| state.space_row(id))
                            .map(|s| s.display_name().to_string())
                            .unwrap_or_default(),
                    })
                    .collect()
            }
        }
    }

    /// The panel for the form; `None` when the shortcut it edits is gone.
    pub(super) fn render_shortcut_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let id = match item {
            PanelItem::NewShortcut => None,
            PanelItem::Shortcut(id) => Some(id.clone()),
            _ => return None,
        };
        if self
            .board
            .shortcuts
            .form
            .as_ref()
            .map(ShortcutForm::item)
            .as_ref()
            != Some(item)
        {
            // A panel opened some other way (or restored): a fresh form.
            let form = self.new_shortcut_form(id, cx)?;
            self.board.shortcuts.form = Some(form);
        }
        let form = self.board.shortcuts.form.as_ref()?;
        let head = PanelHead {
            glyph: form
                .icon
                .as_deref()
                .and_then(icon_asset)
                .or_else(|| form.steps.first().map(|step| step.pick.glyph()))
                .unwrap_or(icons::BOLT),
            tint: None,
            title: match form.id {
                Some(_) => "Edit shortcut",
                None => "New shortcut",
            }
            .into(),
        };
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(parts::field_label(theme, "Name"))
            .child(parts::field(&form.name))
            .child(parts::field_label(theme, "Icon"))
            .child(self.render_icon_grid(form.icon.as_deref(), theme, cx))
            .child(parts::field_label(theme, "Steps"));
        let count = form.steps.len();
        let menu = form.menu;
        let mut boxes = Vec::with_capacity(count);
        for (ix, step) in form.steps.iter().enumerate() {
            boxes.push(self.render_step_box(bot, ix, count, step, menu, theme, cx));
        }
        body = body.child(div().flex().flex_col().gap(px(6.0)).children(boxes));
        if count < SHORTCUT_STEPS_MAX {
            body = body.child(self.render_add_step(menu == Some(FormMenu::AddStep), theme, cx));
        }
        let form = self.board.shortcuts.form.as_ref()?;
        let (footer, cancel, save) = parts::form_footer(
            theme,
            "bot-shortcut-form",
            form.error.clone(),
            match (form.id.is_none(), form.busy) {
                (_, true) => "Saving…",
                (true, false) => "Create shortcut",
                (false, false) => "Save",
            },
            form.busy,
        );
        self.board.panel_footer = Some(
            footer
                .child(cancel.on_click(cx.listener(|this, _, _, cx| this.close_shortcut_form(cx))))
                .child(save.on_click(cx.listener(|this, _, _, cx| this.save_shortcut_form(cx))))
                .into_any_element(),
        );
        Some((head, body.pb(px(16.0))))
    }

    /// The icon choices: a click picks one, a click on the picked one goes
    /// back to the first step's.
    fn render_icon_grid(
        &self,
        picked: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let cells = SHORTCUT_ICONS.iter().map(|&(name, asset)| {
            let on = picked == Some(name);
            div()
                .id(SharedString::from(format!("bot-shortcut-icon-{name}")))
                .size(px(30.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(on, |el| {
                    el.bg(crate::kit::theme::card_selected_bg())
                        .shadow(crate::kit::theme::card_selected_shadows())
                })
                .when(!on, |el| el.hover(|s| s.bg(theme.element_hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.with_shortcut_form(cx, |form| {
                        form.icon = (form.icon.as_deref() != Some(name)).then(|| name.to_string());
                    })
                }))
                .child(icon(asset).size(px(15.0)).text_color(if on {
                    theme.text
                } else {
                    theme.text_muted
                }))
        });
        div()
            .mx(px(8.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(4.0))
            .children(cells)
    }

    /// One step in its box: its number, kind and the buttons that move or
    /// remove it, over its fields.
    #[allow(clippy::too_many_arguments)]
    fn render_step_box(
        &self,
        bot: &Bot,
        ix: usize,
        count: usize,
        step: &StepForm,
        menu: Option<FormMenu>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pick = &step.pick;
        let button = |key: &str, glyph: &'static str, tip: &'static str, enabled: bool| {
            let key = SharedString::from(format!("bot-shortcut-step-{ix}-{key}"));
            div()
                .id(key.clone())
                .flex_none()
                .size(px(22.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .when(!enabled, |el| el.opacity(0.35))
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .bg(motion::hover_blend(
                            &key,
                            crate::kit::theme::wash(0.0),
                            crate::kit::theme::wash(0.12),
                        ))
                        .on_hover(motion::hover_listener(key.clone()))
                        .tooltip(move |_, cx| {
                            cx.new(|_| super::super::session::FindTooltip(tip.into()))
                                .into()
                        })
                })
                .child(icon(glyph).size(px(11.0)).text_color(theme.text_muted))
        };
        let up = button("up", icons::ARROW_UP, "Move up", ix > 0).when(ix > 0, |el| {
            el.on_click(cx.listener(move |this, _, _, cx| {
                this.with_shortcut_form(cx, |form| form.steps.swap(ix, ix - 1))
            }))
        });
        let down = button("down", icons::ARROW_DOWN, "Move down", ix + 1 < count).when(
            ix + 1 < count,
            |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    this.with_shortcut_form(cx, |form| form.steps.swap(ix, ix + 1))
                }))
            },
        );
        let remove = button("remove", icons::CLOSE, "Remove", true).on_click(cx.listener(
            move |this, _, _, cx| {
                this.with_shortcut_form(cx, |form| {
                    form.steps.remove(ix);
                    form.menu = None;
                })
            },
        ));
        let head = div()
            .h(px(36.0))
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from((ix + 1).to_string())),
            )
            .child(
                icon(pick.glyph())
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .child(SharedString::from(kind_title(pick.kind))),
            )
            .child(up)
            .child(down)
            .child(remove);
        let mut fields = div()
            .px(px(12.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0));
        if pick.kind == StepKind::Open {
            fields = fields.child(parts::segmented(
                &format!("bot-shortcut-step-{ix}-open"),
                &[
                    ((ix, OpenKind::Project), "Project"),
                    ((ix, OpenKind::Session), "Session"),
                    ((ix, OpenKind::Url), "Web page"),
                ],
                (ix, pick.open),
                |this, (ix, open), cx| {
                    this.with_shortcut_form(cx, |form| {
                        if let Some(step) = form.steps.get_mut(ix) {
                            step.pick.open = open;
                        }
                        form.menu = None;
                    })
                },
                theme,
                cx,
            ));
        }
        if let Some(picks) = pick.picks() {
            fields = fields.child(self.render_step_picker(
                bot,
                ix,
                picks,
                pick.picked(),
                menu == Some(FormMenu::Pick(ix)),
                theme,
                cx,
            ));
        }
        if pick.has_text() {
            fields = fields.child(parts::field(&step.text).mx_0()).child(
                div()
                    .text_size(px(11.0))
                    .line_height(px(15.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!(
                        "Write {SHORTCUT_INPUT} where each click should ask you for text."
                    ))),
            );
        }
        if pick.kind == StepKind::Open && pick.open == OpenKind::Url {
            fields = fields.child(parts::field(&step.url).mx_0());
        }
        if pick.kind == StepKind::Unknown {
            fields = fields.child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(
                        "Made in a newer version of Cypher, so it's skipped here.",
                    )),
            );
        }
        parts::group_box().mx_0().child(head).child(fields)
    }

    /// A step's picker: a bordered trigger naming the pick (or asking for
    /// one), opening the choices under it.
    #[allow(clippy::too_many_arguments)]
    fn render_step_picker(
        &self,
        bot: &Bot,
        ix: usize,
        picks: Picks,
        picked: Option<&str>,
        open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let choices = self.step_choices(bot, picks, theme, cx);
        let names = self.step_names(cx);
        let ask = match picks {
            Picks::Project => "Choose a project",
            Picks::Routine => "Choose a routine",
            Picks::Session => "Choose a session",
        };
        // A pick no longer offered (a session past the recent ones) still
        // reads by name; one that is gone says so.
        let (glyph, tint, label, label_color) = match picked {
            None => (None, theme.text_muted, ask.to_string(), theme.text_muted),
            Some(id) => match choices.iter().find(|c| c.id == id) {
                Some(choice) => (
                    Some(choice.glyph),
                    choice.tint,
                    choice.label.clone(),
                    theme.text,
                ),
                None => match names.get(id).cloned() {
                    Some(name) => (None, theme.text_muted, name, theme.text),
                    None => (
                        Some(icons::DANGER_TRIANGLE),
                        theme.danger,
                        "Gone, choose another".to_string(),
                        theme.danger,
                    ),
                },
            },
        };
        let popup = open.then(|| {
            let mut list = div()
                .id(SharedString::from(format!(
                    "bot-shortcut-step-{ix}-choices"
                )))
                .max_h(px(240.0))
                .overflow_y_scroll()
                .flex()
                .flex_col();
            if choices.is_empty() {
                list = list.child(
                    div()
                        .px(px(8.0))
                        .py(px(6.0))
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(match picks {
                            Picks::Project => "The workspace sees no projects.",
                            Picks::Routine => "The workspace has no routines yet.",
                            Picks::Session => "No sessions in its projects yet.",
                        })),
                );
            }
            for choice in choices {
                let on = picked == Some(choice.id.as_str());
                let key = format!("bot-shortcut-step-{ix}-choice-{}", choice.id);
                let id = choice.id.clone();
                list = list.child(
                    popover::menu_row(theme, on, key.clone())
                        .id(SharedString::from(key))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.with_shortcut_form(cx, |form| {
                                if let Some(step) = form.steps.get_mut(ix) {
                                    step.pick.pick(id.clone());
                                }
                                form.menu = None;
                            })
                        }))
                        .child(icon(choice.glyph).size(px(14.0)).text_color(choice.tint))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(choice.label)),
                        )
                        .when(!choice.note.is_empty(), |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .max_w(px(110.0))
                                    .truncate()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(choice.note)),
                            )
                        })
                        .when(on, |el| {
                            el.child(icon(icons::CHECK).size(px(12.0)).text_color(theme.text))
                        }),
                );
            }
            let menu = popover::popover_card(theme)
                .w(px(280.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if let Some(form) = this.board.shortcuts.form.as_mut() {
                        form.menu = None;
                    }
                    cx.notify();
                }))
                .child(list);
            popover::anchored_menu_below(
                SharedString::from(format!("bot-shortcut-step-{ix}-popup")),
                menu.into_any_element(),
                None,
            )
        });
        div()
            .id(SharedString::from(format!("bot-shortcut-step-{ix}-pick")))
            .relative()
            .h(px(32.0))
            .px(px(10.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(crate::kit::theme::hairline(0.08))
            .bg(crate::kit::theme::ink(0.04))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(crate::kit::theme::ink(0.06)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.with_shortcut_form(cx, |form| {
                    form.menu =
                        (form.menu != Some(FormMenu::Pick(ix))).then_some(FormMenu::Pick(ix));
                })
            }))
            .when_some(glyph, |el, glyph| {
                el.child(icon(glyph).size(px(13.0)).flex_none().text_color(tint))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.5))
                    .text_color(label_color)
                    .child(SharedString::from(label)),
            )
            .child(
                icon(icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .children(popup)
    }

    /// "+ Add step", opening the four kinds of step.
    fn render_add_step(&self, open: bool, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let popup = open.then(|| {
            let rows = [
                (StepKind::Ask, "add-ask"),
                (StepKind::Session, "add-session"),
                (StepKind::Routine, "add-routine"),
                (StepKind::Open, "add-open"),
            ]
            .into_iter()
            .map(|(kind, key)| {
                let key = format!("bot-shortcut-{key}");
                let title = match kind {
                    StepKind::Open => "Open…",
                    _ => kind_title(kind),
                };
                popover::menu_row(theme, false, key.clone())
                    .id(SharedString::from(key))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let step = new_step_form(
                            StepDraft {
                                pick: StepPick::new(kind),
                                text: String::new(),
                                url: String::new(),
                            },
                            cx,
                        );
                        this.with_shortcut_form(cx, |form| {
                            form.steps.push(step);
                            form.menu = None;
                        })
                    }))
                    .child(
                        icon(kind_glyph(kind, OpenKind::Project))
                            .size(px(16.0))
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from(title))
            });
            let menu = popover::popover_card(theme)
                .w(px(210.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    if let Some(form) = this.board.shortcuts.form.as_mut() {
                        form.menu = None;
                    }
                    cx.notify();
                }))
                .flex()
                .flex_col()
                .children(rows);
            popover::anchored_menu_below("bot-shortcut-add-popup", menu.into_any_element(), None)
        });
        div().mx(px(4.0)).mt(px(6.0)).flex().child(
            parts::action(
                "bot-shortcut-add-step",
                icons::PLUS,
                "Add step",
                theme.text_muted,
            )
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                this.with_shortcut_form(cx, |form| {
                    form.menu = (form.menu != Some(FormMenu::AddStep)).then_some(FormMenu::AddStep);
                })
            }))
            .children(popup),
        )
    }
}

/// A step's form: its picks, and its message and address fields.
fn new_step_form(draft: StepDraft, cx: &mut Context<Shell>) -> StepForm {
    let placeholder = match draft.pick.kind {
        StepKind::Session => "The session's first message",
        _ => "What should the workspace do?",
    };
    let text = cx.new(|cx| TextInput::settings_prompt_field(placeholder, cx));
    let url = cx.new(|cx| TextInput::settings_field("https://", false, cx));
    text.update(cx, |input, cx| input.set_text(draft.text, cx));
    url.update(cx, |input, cx| input.set_text(draft.url, cx));
    StepForm {
        pick: draft.pick,
        text,
        url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(id: &str, order: i64, steps: Vec<ShortcutStep>) -> BotShortcut {
        BotShortcut {
            id: id.into(),
            bot_id: "b".into(),
            label: id.into(),
            icon: None,
            steps,
            order,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn ask(text: &str) -> ShortcutStep {
        ShortcutStep::Ask { text: text.into() }
    }

    fn draft(step: &ShortcutStep) -> StepDraft {
        StepDraft::from_step(step)
    }

    #[test]
    fn the_input_fills_every_blank() {
        let step = ShortcutStep::Session {
            space_id: "web".into(),
            text: "Fix {input}, then test {input}".into(),
        };
        assert_eq!(
            fill_step(&step, " #42 "),
            ShortcutStep::Session {
                space_id: "web".into(),
                text: "Fix #42, then test #42".into(),
            }
        );
        assert_eq!(fill_step(&ask("Plain"), "x"), ask("Plain"));
        let routine = ShortcutStep::Routine {
            routine_id: "r".into(),
        };
        assert_eq!(fill_step(&routine, "x"), routine);
    }

    #[test]
    fn steps_read_in_a_few_words() {
        let names = |id: &str| match id {
            "web" => Some("Web".to_string()),
            "r" => Some("Morning brief".to_string()),
            _ => None,
        };
        assert_eq!(
            step_summary(&ask("What's\nstuck?"), names),
            "Ask \u{201C}What's stuck?\u{201D}"
        );
        assert_eq!(
            step_summary(
                &ShortcutStep::Session {
                    space_id: "web".into(),
                    text: "Go".into()
                },
                names
            ),
            "New session in Web"
        );
        assert_eq!(
            step_summary(
                &ShortcutStep::Routine {
                    routine_id: "gone".into()
                },
                names
            ),
            "Run a routine"
        );
        assert_eq!(
            step_summary(
                &ShortcutStep::Open {
                    target: OpenTarget::Url {
                        url: "https://ci.example.com/runs?x=1".into()
                    }
                },
                names
            ),
            "Open ci.example.com"
        );
        let long = "a".repeat(60);
        assert_eq!(step_summary(&ask(&long), names).chars().count(), 40 + 6);
        assert_eq!(
            failure_line("Morning", 2, StepKind::Routine, "That routine was deleted."),
            "Morning stopped at step 2, run a routine: That routine was deleted."
        );
    }

    #[test]
    fn icons_fall_back_to_the_first_steps() {
        let mut s = shortcut(
            "a",
            0,
            vec![ShortcutStep::Routine {
                routine_id: "r".into(),
            }],
        );
        assert_eq!(shortcut_glyph(&s), icons::REFRESH);
        s.icon = Some("rocket".into());
        assert_eq!(shortcut_glyph(&s), icons::ROCKET);
        s.icon = Some("unheard-of".into());
        assert_eq!(shortcut_glyph(&s), icons::REFRESH);
        s.steps.clear();
        assert_eq!(shortcut_glyph(&s), icons::BOLT);
    }

    #[test]
    fn moves_swap_neighbours_and_stop_at_the_ends() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(moved(&ids, "b", -1).unwrap(), ["b", "a", "c"]);
        assert_eq!(moved(&ids, "b", 1).unwrap(), ["a", "c", "b"]);
        assert_eq!(moved(&ids, "a", -1), None);
        assert_eq!(moved(&ids, "c", 1), None);
        assert_eq!(moved(&ids, "gone", 1), None);
        let (a, b) = (shortcut("a", 3, vec![]), shortcut("b", 7, vec![]));
        assert_eq!(next_order(&[&a, &b]), 8);
        assert_eq!(next_order(&[]), 0);
    }

    #[test]
    fn steps_round_trip_through_the_form() {
        for step in [
            ask("Summarize {input}"),
            ShortcutStep::Session {
                space_id: "web".into(),
                text: "Run the tests".into(),
            },
            ShortcutStep::Routine {
                routine_id: "r".into(),
            },
            ShortcutStep::Open {
                target: OpenTarget::Project {
                    space_id: "web".into(),
                },
            },
            ShortcutStep::Open {
                target: OpenTarget::Session {
                    chat_id: "c".into(),
                },
            },
            ShortcutStep::Open {
                target: OpenTarget::Url {
                    url: "https://ci.example".into(),
                },
            },
            ShortcutStep::Unknown,
        ] {
            assert_eq!(draft(&step).build(), Ok(step.clone()));
        }
        // Switching what an Open step opens keeps the other picks aside.
        let mut open = draft(&ShortcutStep::Open {
            target: OpenTarget::Project {
                space_id: "web".into(),
            },
        });
        open.pick.open = OpenKind::Session;
        assert_eq!(open.build(), Err("a session"));
        open.pick.pick("c".into());
        open.pick.open = OpenKind::Project;
        assert_eq!(open.pick.picked(), Some("web"));
    }

    #[test]
    fn forms_build_checked_shortcuts() {
        let session = StepDraft {
            pick: StepPick::new(StepKind::Session),
            text: " Go ".into(),
            url: String::new(),
        };
        assert_eq!(
            build_shortcut("Go", None, std::slice::from_ref(&session), None, "b", 4, 9),
            Err("Step 1 needs a project.".into())
        );
        let mut session = session;
        session.pick.pick("web".into());
        let built = build_shortcut(
            " Ship it ",
            Some("rocket".into()),
            &[session.clone()],
            None,
            "b",
            4,
            9,
        )
        .unwrap();
        assert_eq!(
            (built.label.as_str(), built.order, built.created_at),
            ("Ship it", 4, 9)
        );
        assert_eq!(
            built.steps,
            [ShortcutStep::Session {
                space_id: "web".into(),
                text: "Go".into()
            }]
        );
        // An edit keeps its id, place and birth.
        let existing = shortcut("s", 2, vec![]);
        let edited = build_shortcut("Ship", None, &[session], Some(&existing), "b", 4, 9).unwrap();
        assert_eq!(
            (
                edited.id.as_str(),
                edited.order,
                edited.created_at,
                edited.updated_at
            ),
            ("s", 2, 1, 9)
        );
        assert_eq!(
            build_shortcut(" ", None, &[draft(&ask("Hi"))], None, "b", 0, 0),
            Err("Give the shortcut a name.".into())
        );
        assert_eq!(
            build_shortcut("Hi", None, &[draft(&ask("  "))], None, "b", 0, 0),
            Err("A message step needs some text.".into())
        );
        assert_eq!(
            build_shortcut("Hi", None, &[], None, "b", 0, 0),
            Err("Add at least one step.".into())
        );
    }
}
