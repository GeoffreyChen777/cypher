//! Routes, settings sections and back/forward route history.

use super::*;

impl Shell {
    /// Close the user menu through the exit animation (no-op when closed).
    pub(super) fn close_user_menu(&mut self, cx: &mut Context<Self>) {
        if self.user_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.user_menu);
            cx.notify();
        }
    }

    /// Close the session-row context menu through the exit animation.
    pub(super) fn close_chat_menu(&mut self, cx: &mut Context<Self>) {
        if self.chat_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.chat_menu);
            cx.notify();
        }
    }

    pub(super) fn dismiss_comment_popup(&mut self, cx: &mut Context<Self>) {
        self.comment_popup
            .update(cx, |popup, cx| popup.dismiss_and_clear(cx));
    }

    pub(super) fn showing_setup(&self) -> bool {
        crate::settings::setup::setup_should_show(
            self.settings.pi_runtime_setup_version >= 1,
            self.debug_setup,
            self.setup_dismissed,
        )
    }

    fn ensure_setup_page(&mut self, cx: &mut Context<Self>) {
        if self.setup_page.is_some() {
            return;
        }
        let state = self.state.clone();
        let page = cx.new(|cx| SetupPage::new(state, cx));
        self.setup_sub = Some(cx.subscribe(&page, |this, _, event: &SetupEvent, cx| {
            this.complete_setup(cx);
            if matches!(event, SetupEvent::ConfigureProviders) {
                this.open_providers(ProviderIntent::Add, cx);
            }
        }));
        self.setup_page = Some(page);
    }

    fn complete_setup(&mut self, cx: &mut Context<Self>) {
        self.settings.setup_completed = true;
        self.settings.pi_runtime_setup_version = 1;
        self.setup_dismissed = true;
        self.setup_page = None;
        self.setup_sub = None;
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn render_setup_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_setup_page(cx);
        let theme = Theme::of(cx).clone();
        let inner = match &self.setup_page {
            Some(page) => page.clone().into_any_element(),
            None => Empty.into_any_element(),
        };
        div()
            .id("setup-overlay")
            .absolute()
            .inset_0()
            .size_full()
            .occlude()
            .bg(theme.bg)
            .child(grid_backdrop(&theme))
            .child(
                div()
                    .id("setup-overlay-body")
                    .absolute()
                    .inset_0()
                    .size_full()
                    .min_h_0()
                    .child(inner),
            )
            .into_any_element()
    }

    pub(super) fn open_providers(&mut self, intent: ProviderIntent, cx: &mut Context<Self>) {
        if self.is_project_window() {
            let target = self.settings_target.read(cx).id().map(str::to_string);
            self.forward_to_main(cx, move |main, cx| {
                main.aim_settings_target(target, cx);
                main.open_providers(intent, cx);
            });
            return;
        }
        self.open_settings(SettingsSection::Providers, cx);
        let state = self.state.clone();
        let target = self.settings_target.clone();
        self.providers_page = Some(cx.new(|cx| ProvidersPage::new(state, target, intent, cx)));
    }

    pub(super) fn open_settings(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        // Settings are app-wide: a project window opens them in the main
        // window, carrying over the device its composer aimed them at.
        if self.is_project_window() {
            let target = self.settings_target.read(cx).id().map(str::to_string);
            self.forward_to_main(cx, move |main, cx| {
                main.aim_settings_target(target, cx);
                main.open_settings(section, cx);
            });
            return;
        }
        if let Some(page) = &self.providers_page {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        if section == SettingsSection::Providers {
            self.providers_page = None;
        }
        if section == SettingsSection::Titles {
            self.titles_page = None;
        }
        // Persisted chat preferences live in the global, not in this editor.
        // Re-enter without a stale font popup or an unfinished HEX draft.
        if section == SettingsSection::Appearance {
            self.appearance_page = None;
        }
        // Recreate per visit: the page's ListHarnesses load re-probes which
        // CLIs are installed, so installing one shows up on the next open.
        if section == SettingsSection::Harnesses {
            self.harnesses_page = None;
        }
        if section == SettingsSection::Commands {
            self.commands_page = None;
        }
        if section == SettingsSection::Mcp {
            self.mcp_page = None;
        }
        // Same reason as the pages above: the profiles are files on the target
        // device, so a fresh visit re-reads them.
        if section == SettingsSection::Subagents {
            self.subagents_page = None;
        }
        // Re-read the sign-in on every visit: a `gh auth login` in a terminal
        // or an expired token should show without restarting.
        if section == SettingsSection::Github {
            self.github_page = None;
        }
        self.dismiss_comment_popup(cx);
        self.route = Route::Settings(section);
        self.nav.push(NavEntry::Settings(section));
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Point the settings device selector at `device` (a forwarded request
    /// from a project window). `None` keeps the current pick.
    fn aim_settings_target(&mut self, device: Option<String>, cx: &mut Context<Self>) {
        if device.is_some() {
            let result = self
                .settings_target
                .update(cx, |target, cx| target.select(device, cx));
            if let Err(error) = result {
                tracing::debug!(%error, "settings target unchanged");
            }
        }
    }

    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = &self.providers_page {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        self.route = Route::Chat;
        self.nav.push(NavEntry::Chat(self.focused_chat_key()));
        cx.notify();
    }

    // ---- back/forward (route history) ----

    pub(super) fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.back() {
            self.apply_nav(entry, cx);
        }
    }

    pub(super) fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.nav.forward() {
            self.apply_nav(entry, cx);
        }
    }

    /// Land on a history entry WITHOUT recording a new one: the stack already
    /// points at `entry` (back/forward moved the index); the selection change
    /// this triggers dedups against `current()` in [`Self::on_state_changed`].
    fn apply_nav(&mut self, entry: NavEntry, cx: &mut Context<Self>) {
        if let Some(page) = &self.providers_page {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        match entry {
            NavEntry::Chat(chat_id) => {
                self.route = Route::Chat;
                // Land on the entry's tab (reopening it if it was closed);
                // the follow push then dedups against `current()`.
                self.open_nav_target(chat_id, cx);
            }
            NavEntry::Settings(section) => {
                self.dismiss_comment_popup(cx);
                self.route = Route::Settings(section);
            }
        }
        self.close_user_menu(cx);
        self.close_chat_menu(cx);
        cx.notify();
    }

    /// Lazily create the entity for a settings section and return it renderable.
    pub(super) fn settings_outlet(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match section {
            SettingsSection::Titles => {
                if self.titles_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.titles_page = Some(
                        cx.new(|cx| crate::settings::titles::TitlesPage::new(state, target, cx)),
                    );
                }
                self.titles_page
                    .as_ref()
                    .unwrap()
                    .clone()
                    .into_any_element()
            }
            SettingsSection::Providers => {
                if self.providers_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.providers_page = Some(
                        cx.new(|cx| ProvidersPage::new(state, target, ProviderIntent::List, cx)),
                    );
                }
                self.providers_page
                    .as_ref()
                    .unwrap()
                    .clone()
                    .into_any_element()
            }
            SettingsSection::Devices => {
                if self.devices_page.is_none() {
                    let state = self.state.clone();
                    self.devices_page = Some(cx.new(|cx| DevicesPage::new(state, cx)));
                }
                match &self.devices_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Harnesses => {
                if self.harnesses_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.harnesses_page = Some(cx.new(|cx| HarnessesPage::new(state, target, cx)));
                }
                match &self.harnesses_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Agents => {
                if self.accounts_page.is_none() {
                    let state = self.state.clone();
                    self.accounts_page = Some(cx.new(|cx| AccountsPage::new(state, cx)));
                }
                match &self.accounts_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Commands => {
                if self.commands_page.is_none() {
                    let state = self.state.clone();
                    let shown = self.settings.shown_slash_commands.clone();
                    let target = self.settings_target.clone();
                    let page = cx.new(|cx| CommandsPage::new(state, target, shown, cx));
                    self.commands_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &CommandsEvent, cx| {
                            let CommandsEvent::Changed(shown) = event;
                            this.settings.shown_slash_commands = shown.clone();
                            crate::settings::commands::publish_shown(shown.clone(), cx);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.commands_page = Some(page);
                }
                match &self.commands_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Mcp => {
                if self.mcp_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.mcp_page = Some(cx.new(|cx| McpPage::new(state, target, cx)));
                }
                match &self.mcp_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Subagents => {
                if self.subagents_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.subagents_page = Some(cx.new(|cx| SubagentsPage::new(state, target, cx)));
                }
                match &self.subagents_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Github => {
                if self.github_page.is_none() {
                    let state = self.state.clone();
                    let target = self.settings_target.clone();
                    self.github_page = Some(
                        cx.new(|cx| crate::settings::github::GithubPage::new(state, target, cx)),
                    );
                }
                match &self.github_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Appearance => {
                if self.appearance_page.is_none() {
                    self.appearance_page = Some(cx.new(AppearancePage::new));
                }
                match &self.appearance_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Notifications => {
                if self.notifications_page.is_none() {
                    let page = cx.new(|cx| {
                        NotificationsPage::new(
                            self.settings.sound_enabled,
                            self.settings.notifications_enabled,
                            self.settings.notifications_background_only,
                            self.settings.dock_badge_enabled,
                            cx,
                        )
                    });
                    // Persist the flags whenever the page flips one.
                    self.notifications_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &NotificationsEvent, cx| {
                            let NotificationsEvent::Changed {
                                sound,
                                desktop,
                                background_only,
                                dock_badge,
                            } = *event;
                            this.settings.sound_enabled = sound;
                            this.settings.notifications_enabled = desktop;
                            this.settings.notifications_background_only = background_only;
                            this.settings.dock_badge_enabled = dock_badge;
                            this.sync_dock_badge(cx);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.notifications_page = Some(page);
                }
                match &self.notifications_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Shortcuts => {
                if self.shortcuts_page.is_none() {
                    let state = self.state.clone();
                    let keymap = self.settings.keymap.clone();
                    let page = cx.new(|cx| ShortcutsPage::new(state, keymap, cx));
                    // Persist + re-apply the keymap whenever the page changes it.
                    self.shortcuts_sub = Some(cx.subscribe(
                        &page,
                        |this: &mut Shell, _, event: &ShortcutsEvent, cx| {
                            let ShortcutsEvent::Changed(keymap) = event;
                            this.settings.keymap = keymap.clone();
                            apply_keymap(cx, keymap);
                            this.schedule_save(cx);
                            cx.notify();
                        },
                    ));
                    self.shortcuts_page = Some(page);
                }
                match &self.shortcuts_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
            SettingsSection::Archived => {
                if self.archived_page.is_none() {
                    let state = self.state.clone();
                    self.archived_page = Some(cx.new(|cx| ArchivedPage::new(state, cx)));
                }
                match &self.archived_page {
                    Some(page) => page.clone().into_any_element(),
                    None => Empty.into_any_element(),
                }
            }
        }
    }
}
