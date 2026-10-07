//! Read-only queries over [`AppState`].

use super::*;

impl AppState {
    /// Non-archived, NON-CHILD chats in sidebar order. Cypher child subagent
    /// chats (engine-owned `child: Some(..)` rows) are hidden from the root
    /// sidebar/session overview — they are reached only through the parent's
    /// Subagents inspector. [`Self::selected_chat_row`] and transcript
    /// subscriptions still work for a selected child (navigation selects it
    /// directly).
    ///
    /// Scoped to this window's projects ([`ProjectScope`]).
    pub fn visible_chats(&self) -> impl Iterator<Item = &Chat> {
        self.chats
            .iter()
            .filter(|c| !c.archived && !c.is_child() && self.scope.chat_visible(c))
    }

    pub fn selected_space_row(&self) -> Option<&Space> {
        if self.no_project {
            return None;
        }
        let id = self.selected_space.as_deref()?;
        self.spaces.iter().find(|s| s.id == id)
    }

    /// The device the new-session canvas targets: the picked project's host
    /// when one is selected, else the explicit device pick, else this device.
    pub fn effective_device_id(&self) -> Option<String> {
        if let Some(space) = self.selected_space_row() {
            return Some(space.device_id.clone());
        }
        self.selected_device
            .clone()
            .or_else(|| self.local_device_id.clone())
    }

    /// Pick the composer's target device. Keeps the project pick consistent:
    /// a project on another device can't survive the switch — fall back to
    /// the first project on the new device, else "no project".
    pub fn select_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        let project_moves = self
            .selected_space_row()
            .is_some_and(|s| s.device_id != device_id);
        if project_moves {
            let first = self
                .spaces_sorted()
                .iter()
                .find(|s| s.device_id == device_id)
                .map(|s| s.id.clone());
            self.no_project = first.is_none();
            if first.is_some() {
                self.selected_space = first;
            }
        }
        self.selected_device = Some(device_id);
        cx.notify();
    }

    /// Aim the canvas at a quick chat on `device_id`: project-less, with the
    /// next send minting a scratch folder there. The caller opens the canvas.
    pub fn begin_quick_chat(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.selected_device = Some(device_id);
        self.no_project = true;
        self.scratch_pending = true;
        cx.notify();
    }

    pub fn space_row(&self, space_id: &str) -> Option<&Space> {
        self.spaces.iter().find(|s| s.id == space_id)
    }

    /// The selected space id, but only while it still resolves to a LIVE
    /// Space — a dangling id (project deleted elsewhere) is `None`. Drives
    /// `last_space_id` persistence and the new-session fallback: a dead
    /// selection must never be remembered or re-aimed at.
    pub fn selected_space_if_live(&self) -> Option<String> {
        self.selected_space
            .as_deref()
            .filter(|id| self.space_row(id).is_some())
            .map(str::to_string)
    }

    /// Spaces in display order — case-insensitive alphabetical, the order
    /// the space selectors (the canvas project picker, composer) list rows in.
    /// Ties break on id so the order is stable across renders.
    /// Scoped to this window's projects ([`ProjectScope`]).
    pub fn spaces_sorted(&self) -> Vec<&Space> {
        let mut spaces: Vec<&Space> = self
            .spaces
            .iter()
            .filter(|s| self.scope.space_visible(&s.id))
            .collect();
        spaces.sort_by_key(|s| (s.display_name().to_lowercase(), s.id.clone()));
        spaces
    }

    /// Non-archived chats of a space in tab (creation) order. Chats with a
    /// dangling/missing `space_id` are invisible by construction.
    pub fn chats_in_space(&self, space_id: &str) -> Vec<&Chat> {
        let mut chats: Vec<&Chat> = self
            .visible_chats()
            .filter(|c| c.space_id.as_deref() == Some(space_id))
            .collect();
        sort_tabs(&mut chats);
        chats
    }

    pub fn device_name(&self, device_id: &str) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.id == device_id)
            .map(|d| d.name.as_str())
    }

    /// Host-presence check: is this device's 15s presence heartbeat fresh?
    /// Distinguishes "host offline" (its queued work syncs when it returns)
    /// from slow sync. The local device is trivially online; unknown devices
    /// get the benefit of the doubt (no evidence — don't cry wolf).
    pub fn device_online(&self, device_id: &str, now: DateTime<Utc>) -> bool {
        if self.local_device_id.as_deref() == Some(device_id) {
            return true;
        }
        match self.devices.iter().find(|d| d.id == device_id) {
            Some(d) => crate::settings::devices::device_online(d.last_seen_at, now),
            None => true,
        }
    }

    /// Does the selected space's folder have git? Drives the branch picker and
    /// the diff sidebar (owner-stamped, synced — no RPC).
    pub fn selected_space_git(&self) -> bool {
        self.selected_space_row().is_some_and(|s| s.git_detected)
    }

    /// Full display status for a chat (tab dots, Active list). A send in
    /// flight ([`Self::begin_pending_send`]) reads as Working — the queued
    /// command is as good as running.
    pub fn display_status_for(&self, chat: &Chat, now: DateTime<Utc>) -> ChatIndicator {
        if self.send_pending(&chat.id, now) {
            return ChatIndicator::Working;
        }
        display_status(chat, self.session_for(&chat.id), now)
    }

    /// The sidebar's Sessions list: every non-archived chat of a LIVE space,
    /// on any device — idle included — in pure recency order (status drives
    /// the dot, never the position; see [`sort_active`]).
    pub fn overview_chats(&self, now: DateTime<Utc>) -> Vec<(ChatIndicator, &Chat)> {
        let mut rows: Vec<(ChatIndicator, &Chat)> = self
            .visible_chats()
            .filter(|c| match c.space_id.as_deref() {
                // Project-less sessions are first-class rows.
                None => true,
                Some(id) => self.space_row(id).is_some(),
            })
            .map(|c| (self.display_status_for(c, now), c))
            .collect();
        sort_active(&mut rows);
        rows
    }

    /// The Dock badge: sessions whose sidebar corner asks for you — waiting
    /// on an answer, errored, or finished — with activity newer than the
    /// synced seen marker. Opening a session on ANY device moves that marker,
    /// so a read on the phone takes it off this badge too (and the Worker
    /// clears the phones' badges off the same marker). The host bumps
    /// `lastMessageAt` when a run starts asking or fails, so a question
    /// counts until it's been looked at, not until it's answered.
    ///
    /// App-wide: the Dock icon is shared by every window, so this counts
    /// the sessions of projects open in their own windows too (the
    /// [`ProjectScope`] is ignored).
    pub fn attention_count(&self, now: DateTime<Utc>) -> usize {
        self.chats
            .iter()
            .filter(|c| !c.archived && !c.is_child())
            .filter(|c| match c.space_id.as_deref() {
                None => true,
                Some(id) => self.space_row(id).is_some(),
            })
            .filter(|chat| {
                matches!(
                    self.display_status_for(chat, now),
                    ChatIndicator::AwaitingInput
                        | ChatIndicator::Errored
                        | ChatIndicator::Completed
                ) && chat.unseen()
            })
            .count()
    }

    /// The project-grouped sidebar: one card per live `Space` (empty spaces
    /// included, so project management stays reachable), plus synthetic
    /// cards for project-less chats ("No project", per device) and chats
    /// whose `space_id` names a missing space ("Unavailable project", keyed
    /// by the missing id). Groups with chats are ordered by their newest chat
    /// (the overview recency order, preserved inside each group); empty
    /// spaces are appended deterministically by display name / device / path
    /// / id. Status changes never reorder. Archived and child chats stay
    /// excluded. Pure — see the tests in [`mod tests`] for the exact rules.
    #[cfg(test)]
    pub fn sidebar_groups(&self, now: DateTime<Utc>) -> Vec<SidebarGroup<'_>> {
        self.sidebar_groups_with(now, &SidebarView::default())
    }

    /// [`Self::sidebar_groups`] under the sidebar view menu's filter and
    /// sort. The filter drops cards hosted elsewhere; the sort reorders
    /// cards (and their sessions) with stable sorts so ties keep the
    /// activity order, and pins always lead.
    pub fn sidebar_groups_with(
        &self,
        now: DateTime<Utc>,
        view: &SidebarView,
    ) -> Vec<SidebarGroup<'_>> {
        let mut groups = self.sidebar_groups_unsorted(now);
        if let Some(device) = view.device.as_deref() {
            groups.retain(|g| g.device_id == device);
        }
        merge_scratch_groups(&mut groups);
        match view.sort {
            SidebarSort::Activity => {}
            SidebarSort::Name => {
                groups.sort_by_cached_key(|g| (g.title.to_lowercase(), g.device.to_lowercase()));
            }
            SidebarSort::Device => {
                groups.sort_by_cached_key(|g| g.device.to_lowercase());
            }
            SidebarSort::Date => {
                groups.sort_by_key(|g| std::cmp::Reverse(g.created_at));
            }
        }
        for group in &mut groups {
            match view.sort {
                SidebarSort::Name => group
                    .chats
                    .sort_by_cached_key(|(_, c)| c.title.as_deref().unwrap_or("").to_lowercase()),
                SidebarSort::Date => group
                    .chats
                    .sort_by_key(|(_, c)| std::cmp::Reverse(c.created_at)),
                SidebarSort::Activity | SidebarSort::Device => {}
            }
        }
        // Reversed direction flips cards, and sessions for the sorts that
        // ordered them (Device leaves sessions in activity order).
        if view.reversed {
            groups.reverse();
            if view.sort != SidebarSort::Device {
                for group in &mut groups {
                    group.chats.reverse();
                }
            }
        }
        // Pins: a pinned project leads the list and a pinned session leads
        // its project, each keeping the sort order among themselves.
        groups.sort_by_key(|g| !g.pinned);
        for group in &mut groups {
            group.chats.sort_by_key(|(_, chat)| !chat.pinned);
        }
        groups
    }

    fn sidebar_groups_unsorted(&self, now: DateTime<Utc>) -> Vec<SidebarGroup<'_>> {
        let mut all: Vec<(ChatIndicator, &Chat)> = self
            .visible_chats()
            .map(|c| (self.display_status_for(c, now), c))
            .collect();
        sort_active(&mut all);

        // Fold chats into groups in overview order. A group is keyed by its
        // live space (`s:<id>`), a missing space id (`u:<id>`), or a device
        // (`np:<device id>` for project-less chats). First appearance orders
        // the groups by their newest chat; within a group the overview order
        // is preserved. Status changes leave the keys and order untouched.
        let mut groups: Vec<SidebarGroup> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (status, chat) in all {
            // Quick chats are keyed per device here so the device filter can
            // drop other hosts; `merge_scratch_groups` then folds whatever
            // survives into the single `sc` card.
            let (key, kind) = match chat.space_id.as_deref() {
                None if chat.is_scratch() => {
                    (format!("sc:{}", chat.device_id), SidebarGroupKind::Scratch)
                }
                None => (
                    format!("np:{}", chat.device_id),
                    SidebarGroupKind::NoProject,
                ),
                Some(id) if self.space_row(id).is_some() => {
                    (format!("s:{id}"), SidebarGroupKind::Space)
                }
                Some(id) => (format!("u:{id}"), SidebarGroupKind::Unavailable),
            };
            if let Some(&ix) = index.get(&key) {
                groups[ix].chats.push((status, chat));
                continue;
            }
            index.insert(key.clone(), groups.len());
            let (space, title, path) = match kind {
                SidebarGroupKind::Space => {
                    let space = self
                        .space_row(chat.space_id.as_deref().expect("space kind has an id"))
                        .expect("space kind resolves");
                    (
                        Some(space),
                        space.display_name().to_string(),
                        Some(space.path.clone()),
                    )
                }
                SidebarGroupKind::NoProject => (None, "No project".into(), None),
                SidebarGroupKind::Scratch => (None, "Quick chats".into(), None),
                SidebarGroupKind::Unavailable => (None, "Unavailable project".into(), None),
            };
            let (device, offline) = match space {
                Some(space) => (
                    self.device_name(&space.device_id)
                        .unwrap_or("Unknown device")
                        .to_string(),
                    !self.device_online(&space.device_id, now),
                ),
                None => (
                    self.device_name(&chat.device_id)
                        .unwrap_or("Unknown device")
                        .to_string(),
                    false,
                ),
            };
            let device_id = space
                .map(|s| s.device_id.clone())
                .unwrap_or_else(|| chat.device_id.clone());
            let created_at = space.map(|s| s.created_at).unwrap_or(chat.created_at);
            groups.push(SidebarGroup {
                key,
                kind,
                title,
                path,
                device,
                device_id,
                offline,
                created_at,
                space_id: space.map(|s| s.id.as_str()),
                pinned: space.is_some_and(|s| s.pinned),
                icon: space.and_then(|s| s.icon.clone()),
                color: space.and_then(|s| s.color.clone()),
                chats: vec![(status, chat)],
            });
        }

        // Append live spaces with no visible chats: project management must
        // stay reachable even when a space is quiet. Deterministic order
        // (display name / device / path / id) so an empty space never moves
        // between renders.
        let live: HashSet<&str> = groups
            .iter()
            .filter(|g| g.kind == SidebarGroupKind::Space)
            .filter_map(|g| g.space_id)
            .collect();
        let mut empty: Vec<&Space> = self
            .spaces
            .iter()
            .filter(|s| !live.contains(s.id.as_str()) && self.scope.space_visible(&s.id))
            .collect();
        empty.sort_by(|a, b| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
                .then_with(|| {
                    self.device_name(&a.device_id)
                        .unwrap_or("")
                        .cmp(self.device_name(&b.device_id).unwrap_or(""))
                })
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.id.cmp(&b.id))
        });
        groups.extend(empty.into_iter().map(|space| {
            SidebarGroup {
                key: format!("s:{}", space.id),
                kind: SidebarGroupKind::Space,
                title: space.display_name().to_string(),
                path: Some(space.path.clone()),
                device: self
                    .device_name(&space.device_id)
                    .unwrap_or("Unknown device")
                    .to_string(),
                device_id: space.device_id.clone(),
                offline: !self.device_online(&space.device_id, now),
                created_at: space.created_at,
                space_id: Some(space.id.as_str()),
                pinned: space.pinned,
                icon: space.icon.clone(),
                color: space.color.clone(),
                chats: Vec::new(),
            }
        }));
        groups
    }

    pub fn session_for(&self, chat_id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.chat_id == chat_id)
    }

    /// Staleness-checked status dot for a chat row. A send in flight reads as
    /// Working (see [`Self::display_status_for`]).
    pub fn indicator_for(&self, chat_id: &str, now: DateTime<Utc>) -> Indicator {
        if self.send_pending(chat_id, now) {
            return Indicator::Working;
        }
        effective_indicator(self.session_for(chat_id), now)
    }

    pub fn selected_chat_row(&self) -> Option<&Chat> {
        let id = self.selected_chat.as_deref()?;
        self.chats.iter().find(|c| c.id == id)
    }

    pub fn gate(&self) -> GatePhase {
        gate_phase(&self.connection, self.workspace_scope, self.auth.as_ref())
    }

    pub fn engine(&self) -> Option<&EngineHandle> {
        self.engine.as_ref()
    }

    /// Drop every account-scoped view and subscription after its runtime has
    /// stopped. The next bootstrap must never render rows from the previous
    /// account while the local profile is opening.
    pub fn prepare_runtime_replacement(&mut self, cx: &mut Context<Self>) {
        self.engine = None;
        self.watch_tasks.clear();
        self.transcript_task = None;
        self.commands_task = None;
        self.connection = ConnectionStatus::Connecting;
        self.workspace_scope = None;
        self.auth = None;
        self.devices.clear();
        self.spaces.clear();
        self.chats.clear();
        self.sessions.clear();
        self.selected_space = None;
        self.no_project = false;
        self.scratch_pending = false;
        self.selected_device = None;
        self.selected_chat = None;
        self.auto_selected = false;
        self.chats_synced = false;
        self.spaces_synced = false;
        self.transcript.clear();
        self.commands.clear();
        self.echoes.clear();
        self.bump_transcript();
        self.pending_sends.borrow_mut().clear();
        self.upload_progress = None;
        self.local_device_id = None;
        self.update = None;
        self.pi_update = None;
        cx.notify();
    }
}
