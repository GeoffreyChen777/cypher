//! Hover cards for pull request, issue and commit references in replies
//! ([`crate::markdown::git_ref`]): a short look at what `#123` or `a1b2c3d`
//! is, fetched once through the chat's host (`ResolveGitRef`) and kept a few
//! minutes per chat. A lookup that finds nothing or can't run shows no card
//! — the reference stays a plain link — and never an error.
//!
//! Click: a pull request or issue opens on GitHub; a commit opens as its own
//! diff tab in the tile's dock, as a click on it in the Git history does
//! (its GitHub page when the checkout doesn't have it).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cypher_proto::{
    GitCommitPreview, GitHistoryCommit, GitRef, GitRefResolution, GithubChecksSummary,
    GithubRefState,
};
use cypher_rpc::methods;
use gpui::{AnyElement, App, Context, SharedString, Task, Window, div, point, prelude::*, px};

use crate::kit::theme::{MonoStyled, Theme};
use crate::markdown::git_ref::{self, GitRefHover, GitRefUi};
use crate::state::AppState;

use super::Transcript;

/// Hover this long before a reference opens its card.
const OPEN_DELAY: Duration = Duration::from_millis(350);
/// Leaving both the link and the card closes it after this grace.
const CLOSE_DELAY: Duration = Duration::from_millis(220);
/// A lookup stays fresh this long (the host caches as long again).
const FRESH_FOR: Duration = Duration::from_secs(180);
/// A lookup that never answered is asked again after this.
const PENDING_FOR: Duration = Duration::from_secs(20);
/// Cards kept, per transcript.
const CACHE_BUDGET: usize = 128;
const CARD_WIDTH: f32 = 340.0;

/// Where a chat's references resolve: its host and, in a workspace's own
/// chat, the granted project shown in Git.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct RefContext {
    pub chat_id: String,
    pub target: Option<String>,
    pub root_space_id: Option<String>,
}

/// The selected chat's reference context: only a chat with a git checkout
/// has one (a session in a checkout; a workspace's own chat with a granted
/// project that is one). Without it nothing is detected.
pub(super) fn ref_context(state: &AppState) -> Option<RefContext> {
    let chat = state.selected_chat_row()?;
    let target = (state.local_device_id.as_deref() != Some(chat.device_id.as_str()))
        .then(|| chat.device_id.clone());
    let root_space_id = if state.workspace_chat_selected() {
        state.git_root()?.space_id.clone()
    } else {
        let git = chat.checkout_id.is_some()
            || chat
                .space_id
                .as_deref()
                .and_then(|id| state.space_row(id))
                .is_some_and(|space| space.git_detected);
        if !git {
            return None;
        }
        None
    };
    Some(RefContext {
        chat_id: chat.id.clone(),
        target,
        root_space_id,
    })
}

/// Answers `ResolveGitRef` (the engine; a stub in tests).
pub(crate) type Resolver = Rc<dyn Fn(RefContext, GitRef, &mut App) -> Task<GitRefResolution>>;
/// Opens a web page (the system browser; a stub in tests).
pub(crate) type UrlOpener = Rc<dyn Fn(&str, &mut App)>;
/// Opens a commit in the tile's Git view.
pub(crate) type CommitOpener = Rc<dyn Fn(GitHistoryCommit, &mut Window, &mut App)>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct CardKey {
    context: RefContext,
    git_ref: GitRef,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Lookup {
    Pending,
    Done(GitRefResolution),
}

/// Lookups per (chat, reference), fresh for [`FRESH_FOR`].
#[derive(Default)]
pub(super) struct GitRefCache {
    entries: HashMap<CardKey, (Lookup, Instant)>,
}

impl GitRefCache {
    /// The lookup to use at `now`: `None` when there is none, or it went
    /// stale (ask again).
    pub(super) fn get(&self, key: &CardKey, now: Instant) -> Option<&Lookup> {
        let (lookup, at) = self.entries.get(key)?;
        let age = now.saturating_duration_since(*at);
        let fresh = match lookup {
            Lookup::Pending => age < PENDING_FOR,
            Lookup::Done(_) => age < FRESH_FOR,
        };
        fresh.then_some(lookup)
    }

    pub(super) fn begin(&mut self, key: CardKey, now: Instant) {
        self.insert(key, Lookup::Pending, now);
    }

    pub(super) fn settle(&mut self, key: CardKey, resolution: GitRefResolution, now: Instant) {
        self.insert(key, Lookup::Done(resolution), now);
    }

    fn insert(&mut self, key: CardKey, lookup: Lookup, now: Instant) {
        if self.entries.len() >= CACHE_BUDGET && !self.entries.contains_key(&key) {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (lookup, now));
    }
}

/// The card's state, owned by the transcript.
#[derive(Default)]
pub(crate) struct GitRefCards {
    target: Option<(GitRefHover, CardKey)>,
    shown: bool,
    on_link: bool,
    on_card: bool,
    timer: Option<Task<()>>,
    cache: GitRefCache,
    tasks: HashMap<CardKey, Task<()>>,
    /// A click before its lookup answered: act on it then.
    click_when_resolved: Option<CardKey>,
    ui: Option<GitRefUi>,
    pub(crate) resolver: Option<Resolver>,
    pub(crate) url_opener: Option<UrlOpener>,
    pub(crate) commit_opener: Option<CommitOpener>,
}

/// What a card says, decided from a lookup (no layout).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardModel {
    /// `owner/repo #12`, or the short SHA.
    pub id: String,
    /// The neutral state mark (`Open`, `Draft`, `Merged`, `Closed`).
    pub mark: Option<&'static str>,
    pub title: String,
    /// Smaller lines under the title, each said once.
    pub details: Vec<String>,
}

pub(super) fn state_label(state: GithubRefState) -> &'static str {
    match state {
        GithubRefState::Open => "Open",
        GithubRefState::Draft => "Draft",
        GithubRefState::Merged => "Merged",
        GithubRefState::Closed => "Closed",
    }
}

/// `2 failed, 1 running, 14 passed` — the counts that are not zero, the
/// ones needing attention first; skipped checks only when nothing else ran.
pub(super) fn checks_text(checks: &GithubChecksSummary) -> Option<String> {
    let parts: Vec<String> = [
        (checks.failed, "failed"),
        (checks.pending, "running"),
        (checks.passed, "passed"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}"))
    .collect();
    if parts.is_empty() {
        return (checks.skipped > 0).then(|| format!("Checks: {} skipped", checks.skipped));
    }
    Some(format!("Checks: {}", parts.join(", ")))
}

/// `3 files · +12 −4`.
pub(super) fn diff_stat(files: u64, insertions: u64, deletions: u64) -> String {
    let noun = if files == 1 { "file" } else { "files" };
    format!("{files} {noun} · +{insertions} \u{2212}{deletions}")
}

/// How long ago `then` was at `now` (unix seconds), coarsely.
pub(super) fn relative_time(then: i64, now: i64) -> String {
    let secs = (now - then).max(0);
    let (n, unit) = match secs {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s if s < 86_400 * 30 => (s / 86_400, "day"),
        s if s < 86_400 * 365 => (s / (86_400 * 30), "month"),
        s => (s / (86_400 * 365), "year"),
    };
    if unit == "day" && n == 1 {
        return "yesterday".into();
    }
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// The card a lookup makes, if any (`notFound` / `unavailable`: none).
pub(super) fn card_model(resolution: &GitRefResolution, now: i64) -> Option<CardModel> {
    Some(match resolution {
        GitRefResolution::PullRequest(pull) => CardModel {
            id: format!("{} #{}", pull.repo, pull.number),
            mark: Some(state_label(pull.state)),
            title: pull.title.clone(),
            details: [
                Some(format!(
                    "{} · {} \u{2192} {}",
                    pull.author, pull.head_branch, pull.base_branch
                )),
                pull.checks.as_ref().and_then(checks_text),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
        GitRefResolution::Issue(issue) => CardModel {
            id: format!("{} #{}", issue.repo, issue.number),
            mark: Some(match (issue.state, issue.state_reason.as_deref()) {
                (GithubRefState::Closed, Some("not_planned")) => "Not planned",
                (state, _) => state_label(state),
            }),
            title: issue.title.clone(),
            details: vec![issue.author.clone()],
        },
        GitRefResolution::Commit(commit) => CardModel {
            id: commit.short_sha.clone(),
            mark: None,
            title: commit.subject.clone(),
            details: vec![
                format!(
                    "{} · {}",
                    commit.author,
                    relative_time(commit.author_time, now)
                ),
                diff_stat(
                    commit.files_changed.into(),
                    commit.insertions.into(),
                    commit.deletions.into(),
                ),
            ],
        },
        GitRefResolution::NotFound { .. } | GitRefResolution::Unavailable { .. } => return None,
    })
}

/// The Git view's commit for a looked-up one (its diff tab needs only the
/// SHA and subject).
pub(super) fn history_commit(commit: &GitCommitPreview) -> GitHistoryCommit {
    GitHistoryCommit {
        sha: commit.sha.clone(),
        parent_shas: Vec::new(),
        subject: commit.subject.clone(),
        author_name: commit.author.clone(),
        author_email: String::new(),
        authored_at: chrono::DateTime::from_timestamp(commit.author_time, 0)
            .map(|at| at.to_rfc3339())
            .unwrap_or_default(),
        refs: Vec::new(),
    }
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `ResolveGitRef` through the engine; any failure is `unavailable`.
fn engine_resolver(state: gpui::Entity<AppState>) -> Resolver {
    Rc::new(move |context, git_ref, cx| {
        let engine = state.read(cx).engine().cloned();
        let fallback = GitRefResolution::Unavailable {
            reason: cypher_proto::GitRefUnavailable::Unreachable,
            url: git_ref.web_url(None),
        };
        let Some(engine) = engine else {
            return Task::ready(fallback);
        };
        let mut params = serde_json::Map::new();
        params.insert("chatId".into(), context.chat_id.into());
        if let Some(target) = context.target {
            params.insert("targetDeviceId".into(), target.into());
        }
        if let Some(space) = context.root_space_id {
            params.insert("rootSpaceId".into(), space.into());
        }
        params.insert(
            "ref".into(),
            serde_json::to_value(&git_ref).unwrap_or_default(),
        );
        cx.spawn(async move |_| {
            engine
                .client()
                .call(methods::RESOLVE_GIT_REF, serde_json::Value::Object(params))
                .await
                .ok()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or(fallback)
        })
    })
}

impl Transcript {
    /// Open commits clicked in replies in the tile's Git view.
    pub fn set_commit_opener(&mut self, opener: CommitOpener) {
        self.git_refs.commit_opener = Some(opener);
    }

    /// The reference wiring for the selected chat's rows: `None` without a
    /// git checkout (nothing is detected then).
    pub(super) fn git_ref_ui(&mut self, cx: &mut Context<Self>) -> Option<GitRefUi> {
        ref_context(self.state.read(cx))?;
        if let Some(ui) = &self.git_refs.ui {
            return Some(ui.clone());
        }
        let (hover, click) = (cx.weak_entity(), cx.weak_entity());
        let ui = GitRefUi {
            on_hover: Rc::new(move |link, window, cx| {
                hover
                    .update(cx, |this, cx| this.git_ref_hovered(link, window, cx))
                    .ok();
            }),
            on_click: Rc::new(move |link, window, cx| {
                click
                    .update(cx, |this, cx| this.git_ref_clicked(link, window, cx))
                    .ok();
            }),
        };
        self.git_refs.ui = Some(ui.clone());
        Some(ui)
    }

    fn git_ref_key(&self, git_ref: &GitRef, cx: &App) -> Option<CardKey> {
        Some(CardKey {
            context: ref_context(self.state.read(cx))?,
            git_ref: git_ref.clone(),
        })
    }

    pub(super) fn git_ref_hovered(
        &mut self,
        link: Option<GitRefHover>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = link else {
            self.git_refs.on_link = false;
            if self.git_refs.shown {
                self.schedule_git_ref_close(cx);
            } else {
                self.git_refs.target = None;
                self.git_refs.timer = None;
            }
            return;
        };
        self.git_refs.on_link = true;
        if let Some((target, _)) = &mut self.git_refs.target
            && target.key == link.key
        {
            target.anchor = link.anchor;
            if self.git_refs.shown {
                self.git_refs.timer = None;
            }
            return;
        }
        let Some(key) = self.git_ref_key(&link.git_ref, cx) else {
            return;
        };
        self.git_refs.target = Some((link, key.clone()));
        if self.git_refs.shown {
            self.git_refs.timer = None;
            self.lookup_git_ref(&key, cx);
            cx.notify();
            return;
        }
        // Ask while the pointer rests: the card often opens with its answer.
        self.lookup_git_ref(&key, cx);
        self.git_refs.timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(OPEN_DELAY).await;
            this.update(cx, |this, cx| {
                this.git_refs.timer = None;
                if this.git_refs.target.is_some() && this.git_refs.on_link {
                    this.git_refs.shown = true;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn git_ref_card_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.git_refs.on_card = hovered;
        if hovered {
            self.git_refs.timer = None;
        } else if self.git_refs.shown {
            self.schedule_git_ref_close(cx);
        }
    }

    fn schedule_git_ref_close(&mut self, cx: &mut Context<Self>) {
        self.git_refs.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CLOSE_DELAY).await;
            this.update(cx, |this, cx| {
                this.git_refs.timer = None;
                if !this.git_refs.on_link && !this.git_refs.on_card {
                    this.hide_git_ref(cx);
                }
            })
            .ok();
        }));
    }

    /// Close the card (scrolled, clicked, left).
    pub(super) fn hide_git_ref(&mut self, cx: &mut Context<Self>) {
        let cards = &mut self.git_refs;
        if cards.target.is_none() && !cards.shown {
            return;
        }
        cards.target = None;
        cards.shown = false;
        cards.on_link = false;
        cards.on_card = false;
        cards.timer = None;
        git_ref::clear_hover(self.scope);
        cx.notify();
    }

    /// The reference's lookup, starting it when there is no fresh one.
    fn lookup_git_ref(&mut self, key: &CardKey, cx: &mut Context<Self>) -> Lookup {
        let now = Instant::now();
        if let Some(lookup) = self.git_refs.cache.get(key, now) {
            return lookup.clone();
        }
        let resolver = self
            .git_refs
            .resolver
            .clone()
            .unwrap_or_else(|| engine_resolver(self.state.clone()));
        let answer = resolver(key.context.clone(), key.git_ref.clone(), cx);
        self.git_refs.cache.begin(key.clone(), now);
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let resolution = answer.await;
            this.update(cx, |this, cx| {
                this.git_refs.tasks.remove(&task_key);
                this.git_refs
                    .cache
                    .settle(task_key.clone(), resolution, Instant::now());
                if this.git_refs.click_when_resolved.as_ref() == Some(&task_key) {
                    this.git_refs.click_when_resolved = None;
                    this.act_on_git_ref(&task_key, None, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.git_refs.tasks.insert(key.clone(), task);
        Lookup::Pending
    }

    pub(super) fn git_ref_clicked(
        &mut self,
        link: GitRefHover,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.git_ref_key(&link.git_ref, cx) else {
            return;
        };
        self.hide_git_ref(cx);
        match self.lookup_git_ref(&key, cx) {
            Lookup::Pending => self.git_refs.click_when_resolved = Some(key),
            Lookup::Done(_) => self.act_on_git_ref(&key, Some(window), cx),
        }
    }

    /// A click's destination, once its lookup answered: a commit in this
    /// checkout opens in the Git view, anything else its web page.
    fn act_on_git_ref(&mut self, key: &CardKey, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let Some(Lookup::Done(resolution)) = self.git_refs.cache.get(key, Instant::now()).cloned()
        else {
            return;
        };
        if let (GitRefResolution::Commit(commit), Some(opener)) =
            (&resolution, self.git_refs.commit_opener.clone())
        {
            let commit = history_commit(commit);
            match window {
                Some(window) => window.defer(cx, move |window, cx| opener(commit, window, cx)),
                None => {
                    if let Some(window) = cx.active_window() {
                        cx.defer(move |cx| {
                            window
                                .update(cx, |_, window, cx| opener(commit, window, cx))
                                .ok();
                        });
                    }
                }
            }
            return;
        }
        let url = resolution
            .url()
            .map(str::to_owned)
            .or_else(|| key.git_ref.web_url(None));
        if let Some(url) = url {
            match self.git_refs.url_opener.clone() {
                Some(open) => open(&url, cx),
                None => cx.open_url(&url),
            }
        }
    }

    /// The shown card's model, when its lookup made one.
    pub(super) fn git_ref_card_model(&self) -> Option<CardModel> {
        if !self.git_refs.shown {
            return None;
        }
        let (_, key) = self.git_refs.target.as_ref()?;
        match self.git_refs.cache.get(key, Instant::now())? {
            Lookup::Done(resolution) => card_model(resolution, unix_now()),
            Lookup::Pending => None,
        }
    }

    /// Whether the shown card is still waiting for its lookup.
    fn git_ref_card_loading(&self) -> bool {
        self.git_refs.shown
            && self.git_refs.target.as_ref().is_some_and(|(_, key)| {
                matches!(
                    self.git_refs.cache.get(key, Instant::now()),
                    Some(Lookup::Pending) | None
                )
            })
    }

    /// The card, on the floating layer under (or over) its reference.
    pub(super) fn git_ref_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (hover, _) = self.git_refs.target.clone()?;
        let model = self.git_ref_card_model();
        if model.is_none() && !self.git_ref_card_loading() {
            return None;
        }
        let theme = crate::appearance::chat_style::theme(cx);
        let body = match model {
            Some(model) => card_body(&theme, model),
            None => div()
                .px(px(12.0))
                .py(px(8.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child("Loading…")
                .into_any_element(),
        };
        let viewport = window.viewport_size();
        let width = CARD_WIDTH.min(f32::from(viewport.width) - 16.0).max(200.0);
        let card = crate::kit::popover::popover_card_flush(&theme)
            .id("git-ref-card")
            .w(px(width))
            .occlude()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.git_ref_card_hovered(*hovered, cx);
            }))
            .child(body);
        let card = crate::kit::frost::frosted_always(12.0, crate::kit::frost::MENU_BLUR, card);
        let anchor = hover.anchor;
        let below = f32::from(viewport.height - anchor.bottom());
        let (position, corner) = if below >= 120.0 || below >= f32::from(anchor.top()) {
            (point(anchor.left(), anchor.bottom() + px(4.0)), gpui::Anchor::TopLeft)
        } else {
            (point(anchor.left(), anchor.top() - px(4.0)), gpui::Anchor::BottomLeft)
        };
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(position)
                    .anchor(corner)
                    .snap_to_window_with_margin(px(8.0))
                    .child(card),
            )
            .priority(2)
            .into_any_element(),
        )
    }
}

fn card_body(theme: &Theme, model: CardModel) -> AnyElement {
    let head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .text_size(px(11.5))
        .text_color(theme.text_muted)
        .when_some(model.mark, |el, mark| {
            el.child(
                div()
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .text_size(px(10.5))
                    .child(mark),
            )
        })
        .child(div().mono(theme).child(SharedString::from(model.id)));
    div()
        .px(px(12.0))
        .py(px(9.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(head)
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(SharedString::from(model.title)),
        )
        .children(model.details.into_iter().map(|line| {
            div()
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(SharedString::from(line))
        }))
        .into_any_element()
}

#[cfg(test)]
#[path = "git_refs/tests.rs"]
mod tests;
