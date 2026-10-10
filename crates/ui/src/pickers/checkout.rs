//! Checkout resolution (the t3code env-mode semantics).

use super::*;

impl Pickers {
    /// Index of the highlighted-by-default row in the (filtered) ref list:
    /// the session's branch on an existing chat, the draft pick on a new one,
    /// else the current branch. Capped to the displayed window.
    pub(super) fn selected_ref_index(&self, cx: &App) -> usize {
        let rows = self.filtered_ref_rows(cx);
        let selected = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.branch.clone())
            .or_else(|| self.config.branch.clone());
        let index = match selected {
            Some(name) => rows.iter().position(|r| r.name == name).unwrap_or(0),
            None => rows.iter().position(|r| r.current).unwrap_or(0),
        };
        index.min(MAX_REF_ROWS.saturating_sub(1))
    }

    /// The picked ref's row, else the repo's current branch's row.
    pub(super) fn selected_ref(&self) -> Option<&RepoRef> {
        let refs = self.refs.ready()?;
        match self.config.branch.as_deref() {
            Some(name) => refs.iter().find(|r| r.name == name),
            None => refs.iter().find(|r| r.current),
        }
    }

    /// The picked (or current) ref's name.
    fn effective_ref_name(&self) -> Option<String> {
        self.config
            .branch
            .clone()
            .or_else(|| self.selected_ref().map(|r| r.name.clone()))
    }

    /// The existing worktree the picked ref is materialized in, if any.
    /// A programmatic pin wins here too: the canvas must read as "Current
    /// worktree" (and resolve to its path) even before ListRefs loads.
    pub(super) fn selected_ref_worktree(&self, cx: &App) -> Option<String> {
        if let Some(pin) = self.pinned_plan(cx) {
            return match pin {
                CheckoutPlan::ReuseWorktree { path, .. } => Some(path.clone()),
                _ => None,
            };
        }
        self.selected_ref().and_then(|r| r.worktree_path.clone())
    }

    /// Pure resolution of the on-send checkout plan: a programmatic pin wins
    /// only when it names the canvas's LIVE selected space (the caller passes
    /// `selected_space_row().map(|s| s.id)` — `None` for a no-project or
    /// dangling selection, which can never match); otherwise the derived
    /// refs-based plan. Kept free for focused tests.
    pub(super) fn resolve_checkout_plan(
        pinned: Option<&PinnedCheckout>,
        selected_space: Option<&str>,
        on_canvas: bool,
        derived: CheckoutPlan,
    ) -> CheckoutPlan {
        match pinned {
            Some(pin) if on_canvas && selected_space == Some(pin.space.as_str()) => {
                pin.plan.clone()
            }
            _ => derived,
        }
    }

    /// The active pinned checkout plan, if the pin's space is still the
    /// canvas's LIVE selected project and we're not inside a session. A
    /// "no project" canvas or a dangling space id reads as no pin — the
    /// explicit no-project opt-out and deleted projects both clear it.
    pub(super) fn pinned_plan(&self, cx: &App) -> Option<&CheckoutPlan> {
        let state = self.state.read(cx);
        if state.selected_chat.is_some() {
            return None;
        }
        let pin = self.pinned.as_ref()?;
        state
            .selected_space_row()
            .is_some_and(|s| s.id == pin.space)
            .then_some(&pin.plan)
    }

    /// The resolved on-send checkout action for a new session. A programmatic
    /// pin (sidebar hover add) is authoritative without refs and wins over the
    /// derived plan for its own space.
    pub fn checkout_plan(&self, cx: &App) -> CheckoutPlan {
        let state = self.state.read(cx);
        let derived = match self.config.checkout {
            CheckoutKind::NewWorktree => CheckoutPlan::NewWorktree {
                base: self.effective_ref_name(),
            },
            CheckoutKind::Local => match self.selected_ref_worktree(cx) {
                Some(path) => CheckoutPlan::ReuseWorktree {
                    path,
                    branch: self.effective_ref_name(),
                },
                None => CheckoutPlan::CurrentCheckout {
                    branch: self.effective_ref_name(),
                },
            },
        };
        Self::resolve_checkout_plan(
            self.pinned.as_ref(),
            state.selected_space_row().map(|s| s.id.as_str()),
            state.selected_chat.is_none(),
            derived,
        )
    }

    /// Label of the checkout-kind trigger (t3code `resolveEnvModeLabel` /
    /// `resolveCurrentWorkspaceLabel`). The pinned plan drives the label too,
    /// so a worktree-targeted canvas reads "Current worktree" pre-refs.
    pub(super) fn checkout_label(&self, cx: &App) -> &'static str {
        if let Some(pin) = self.pinned_plan(cx) {
            return match pin {
                CheckoutPlan::NewWorktree { .. } => "New worktree",
                CheckoutPlan::ReuseWorktree { .. } => "Current worktree",
                _ => "Current checkout",
            };
        }
        match self.config.checkout {
            CheckoutKind::NewWorktree => "New worktree",
            CheckoutKind::Local => {
                if self.selected_ref_worktree(cx).is_some() {
                    "Current worktree"
                } else {
                    "Current checkout"
                }
            }
        }
    }

    /// Label of the ref trigger: `From <ref>` only when a NEW worktree will be
    /// created off it (t3code `getBranchTriggerLabel`); the bare name
    /// otherwise. The pinned plan drives BOTH the worktree-ness and the branch
    /// (when known) — it is authoritative over any stale draft
    /// `config.checkout` (a same-project hover-add onto a worktree must read
    /// as its branch name, not "From …").
    pub(super) fn ref_label(&self, cx: &App) -> SharedString {
        let pinned = self.pinned_plan(cx);
        let effective = self.effective_ref_name();
        let (is_new_worktree, branch): (bool, Option<&str>) = match pinned {
            Some(CheckoutPlan::NewWorktree { base }) => (true, base.as_deref()),
            Some(CheckoutPlan::CurrentCheckout { branch }) => (false, branch.as_deref()),
            Some(CheckoutPlan::ReuseWorktree { branch, .. }) => (false, branch.as_deref()),
            None => (
                self.config.checkout == CheckoutKind::NewWorktree,
                effective.as_deref(),
            ),
        };
        Self::ref_label_impl(is_new_worktree, branch)
    }

    /// Pure ref-trigger label (t3code `getBranchTriggerLabel`): `From <ref>`
    /// only when a NEW worktree will be created off it; the bare name
    /// otherwise. Kept free for focused tests.
    pub(super) fn ref_label_impl(is_new_worktree: bool, branch: Option<&str>) -> SharedString {
        match (is_new_worktree, branch) {
            (_, None) => SharedString::from("Select ref"),
            (true, Some(name)) => SharedString::from(format!("From {name}")),
            (false, Some(name)) => SharedString::from(name),
        }
    }

    /// Pure checkout-kind trigger icon: the plain "Current checkout" reads as
    /// a bare folder; a worktree-backed ("Current worktree") or fresh-worktree
    /// target as a folder-with-files. Kept free for focused tests.
    pub(super) fn checkout_kind_icon(is_current_checkout: bool) -> &'static str {
        if is_current_checkout {
            crate::kit::icons::FOLDER
        } else {
            crate::kit::icons::FOLDER_WITH_FILES
        }
    }

    /// Pure transition rule for the owner stamps: a reset runs only when the
    /// current selection differs from the recorded owner. A deferred observer
    /// re-running after `target_checkout` synchronized the stamps sees matching
    /// values here and stays a no-op — it cannot clear the fresh pin.
    pub(super) fn owner_transition_required(
        owner: &Option<String>,
        selected: &Option<String>,
    ) -> bool {
        owner != selected
    }

    /// Pure rule: an explicit "don't work in a project" opt-out must
    /// INVALIDATE (clear) any programmatic checkout pin even when the raw
    /// selected-space id is unchanged — the opt-out leaves the id in place, so
    /// masking via [`Self::pinned_plan`] alone would let a later re-pick of
    /// the same project revive the stale pin. Kept free for focused tests.
    pub(super) fn no_project_invalidates_pin(pinned: bool, no_project: bool) -> bool {
        pinned && no_project
    }

    /// Owner-stamp transition for the branch draft/cache (chat selection): a
    /// change drops the draft picks that belonged to the previous chat or the
    /// new-session canvas. Shared by the state observer (deferred) and
    /// [`Self::target_checkout`] (run synchronously BEFORE the pin is set), so
    /// both apply the exact same transition rule. A no-op when the owner is
    /// unchanged — which is what keeps a deferred observer from re-clearing a
    /// pin whose owner `target_checkout` already synchronized.
    pub(super) fn sync_draft_owner(&mut self, selected: Option<String>) {
        if !Self::owner_transition_required(&self.draft_owner, &selected) {
            return;
        }
        self.draft_owner = selected;
        self.config.harness = None;
        self.config.model = None;
        self.config.reasoning = None;
        self.config.model_options.clear();
        self.switch_error = None;
        // A session selection leaves the new-session canvas — the programmatic
        // checkout pin only targets the canvas, so it clears (a later sidebar
        // `+` re-pins if needed). The mirror it left in the draft goes too.
        self.clear_pinned_target();
    }

    /// Owner-stamp transition for the branch draft/cache (space selection): a
    /// space switch invalidates the branch draft + cache — the folder (and
    /// possibly the device) changed under them. Shared with the state observer;
    /// `target_checkout` runs it synchronously so the deferred observer sees
    /// matching owners (see [`Self::sync_draft_owner`]).
    pub(super) fn sync_space_owner(&mut self, space: Option<String>) {
        if !Self::owner_transition_required(&self.space_owner, &space) {
            return;
        }
        self.space_owner = space;
        self.config.branch = None;
        self.config.checkout = CheckoutKind::default();
        self.refs = Loadable::Idle;
        self.refs_space = None;
        // The pinned checkout is space-scoped: a project change clears it
        // (the pin's space no longer matches the selection). The mirror it
        // left in the draft goes too.
        self.clear_pinned_target();
        // Catalogs are per-DEVICE (fetched from the space's host): a space
        // switch may land on another device, so refetch.
        self.harnesses = Loadable::Idle;
        self.model_generation = self.model_generation.wrapping_add(1);
        self.models.clear();
    }

    /// Pure clear rule for a programmatic checkout target: only when a pin
    /// actually exists does the clear ALSO reset the checkout draft it
    /// mirrored (`branch` → `None`, `checkout` → default) — an ordinary/manual
    /// unpinned draft is preserved untouched. Kept free for focused tests.
    pub(super) fn clear_pinned_target_impl(
        pinned: &mut Option<PinnedCheckout>,
        branch: &mut Option<String>,
        checkout: &mut CheckoutKind,
    ) {
        if pinned.is_none() {
            return;
        }
        *pinned = None;
        *branch = None;
        *checkout = CheckoutKind::default();
    }

    /// Drop any programmatic checkout pin (the sidebar's hover add buttons)
    /// AND the checkout draft it mirrored into `config` — but only when a pin
    /// actually exists, so ordinary/manual unpinned draft selections are
    /// preserved. A cleared pin must never leave its mirror behind: the
    /// refs-derived plan (`checkout_plan`) would otherwise reconstruct the
    /// same worktree from `config.branch` + a worktree-backed ref after a
    /// global New Session or a no-project/reselect.
    pub(super) fn clear_pinned_target(&mut self) {
        Self::clear_pinned_target_impl(
            &mut self.pinned,
            &mut self.config.branch,
            &mut self.config.checkout,
        );
    }

    /// Drop any programmatic checkout pin (the sidebar's hover add buttons).
    /// The generic global "new session" action calls this so an already-pinned
    /// canvas reads generically again; the project/ref/checkout pickers and the
    /// owner transitions use the same helper.
    pub fn clear_checkout_target(&mut self) {
        self.clear_pinned_target();
    }

    /// Programmatic target for the new-session canvas (the sidebar's hover add
    /// buttons): land on `space_id`'s canvas and pin the checkout plan. The pin
    /// is authoritative without refs — no ListRefs round-trip. GPUI state
    /// observers fire DEFERRED, after this method returns — so the owner
    /// stamps are synchronized HERE first (the same transitions the observer
    /// would run), and only then is the fresh pin set. A later observer run
    /// sees matching owners, is a no-op, and cannot wipe the pin.
    pub fn target_checkout(
        &mut self,
        space_id: String,
        plan: CheckoutPlan,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |s, cx| {
            s.select_space(Some(space_id.clone()), cx);
            s.select_chat(None, cx);
        });
        self.sync_draft_owner(None);
        self.sync_space_owner(Some(space_id.clone()));
        // Synchronize the visible draft checkout state with the pinned plan:
        // a same-project hover-add leaves both owners unchanged (no reset), so
        // without this a stale NewWorktree draft/branch would sit behind the
        // fresh pin. The pin stays authoritative in rendering and on-send
        // regardless — this just keeps the chips consistent with it.
        match &plan {
            CheckoutPlan::CurrentCheckout { branch }
            | CheckoutPlan::ReuseWorktree { branch, .. } => {
                self.config.checkout = CheckoutKind::Local;
                self.config.branch = branch.clone();
            }
            CheckoutPlan::NewWorktree { base } => {
                self.config.checkout = CheckoutKind::NewWorktree;
                self.config.branch = base.clone();
            }
        }
        self.pinned = Some(PinnedCheckout {
            space: space_id,
            plan,
        });
        cx.notify();
    }
}
