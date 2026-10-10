//! Which Pi slash commands the composer `/` menu shows, how the Commands page
//! groups them, and the provider commands the composer hands to Settings.
//!
//! The shown names are a device-local preference in `ui-settings.json`
//! ([`super::UiSettings::shown_slash_commands`]), published as the
//! [`ShownSlashCommands`] global; a hidden command still runs if typed.

use gpui::{App, Global};

use crate::kit::icons;

/// The command names the user turned on; every other command stays out of
/// the `/` menu. Names a device no longer offers are kept, so a preference
/// survives switching between devices with different catalogs.
#[derive(Clone, Default)]
pub struct ShownSlashCommands {
    pub names: Vec<String>,
}

impl Global for ShownSlashCommands {}

/// Commands the `/` menu shows until the user turns them off: the ones whose
/// row says what is in effect (a badge). Each is turned on once per device
/// ([`offer_defaults`]), so turning one off sticks and a name added here
/// later still reaches existing installs.
pub const SHOWN_BY_DEFAULT: &[&str] = &[
    "compact",
    "goal",
    "fast",
    "scripts",
    "orchestrate",
    "subagent-status",
];

/// Turn on each [`SHOWN_BY_DEFAULT`] command not yet in `offered`, and record
/// it there.
pub fn offer_defaults(shown: &mut Vec<String>, offered: &mut Vec<String>) {
    for name in SHOWN_BY_DEFAULT {
        if shows(offered, name) {
            continue;
        }
        offered.push((*name).to_string());
        if !shows(shown, name) {
            shown.push((*name).to_string());
        }
    }
}

pub fn shows(shown: &[String], name: &str) -> bool {
    shown.iter().any(|item| item == name)
}

/// `shown` with every name in `names` turned on or off, in a stable order.
pub fn set_visible(shown: &[String], names: &[String], visible: bool) -> Vec<String> {
    let mut list = shown.to_vec();
    for name in names {
        if visible {
            if !shows(&list, name) {
                list.push(name.clone());
            }
        } else {
            list.retain(|item| item != name);
        }
    }
    list
}

pub fn publish_shown(names: Vec<String>, cx: &mut App) {
    cx.set_global(ShownSlashCommands { names });
}

pub fn shows_in_app(cx: &App, name: &str) -> bool {
    cx.try_global::<ShownSlashCommands>()
        .is_some_and(|slot| shows(&slot.names, name))
}

/// Whether any command is turned on at all. With none, the composer never
/// opens the `/` menu (or fetches the list for it).
pub fn any_shown_in_app(cx: &App) -> bool {
    cx.try_global::<ShownSlashCommands>()
        .is_some_and(|slot| !slot.names.is_empty())
}

/// What a command is for — the page's groups, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandGroup {
    Conversation,
    AgentModes,
    Subagents,
    Skills,
    Other,
    SettingsShortcuts,
}

impl CommandGroup {
    pub const ALL: [Self; 6] = [
        Self::Conversation,
        Self::AgentModes,
        Self::Subagents,
        Self::Skills,
        Self::Other,
        Self::SettingsShortcuts,
    ];

    /// The group's name, on this page and in the composer's `/` menu.
    pub fn title(self) -> &'static str {
        match self {
            Self::Conversation => "Conversation",
            Self::AgentModes => "Agent modes",
            Self::Subagents => "Subagents",
            Self::Skills => "Skills",
            Self::Other => "Other",
            Self::SettingsShortcuts => "Settings shortcuts",
        }
    }

    pub fn caption(self) -> &'static str {
        match self {
            Self::Conversation => "Act on the current session.",
            Self::AgentModes => "Change how the agent works.",
            Self::Subagents => "Inspect and manage subagent runs.",
            Self::Skills => "Send a message with one skill's instructions loaded.",
            Self::Other => "From other extensions and prompt templates.",
            Self::SettingsShortcuts => "Open or repeat what a Settings page already does.",
        }
    }
}

/// Where a command sits on the page. `advanced` commands configure something
/// a Settings page already covers, so they wait behind the group's collapsed
/// "Advanced" row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub group: CommandGroup,
    pub advanced: bool,
}

/// Group and advanced flag for a command name. Known commands are placed by
/// name; an unknown `*-config` command counts as configuration, and anything
/// else unknown lands in Other.
pub fn placement(name: &str) -> Placement {
    use CommandGroup::*;
    let name = name.to_ascii_lowercase();
    let at = |group, advanced| Placement { group, advanced };
    match name.as_str() {
        "compact" | "export-html" => at(Conversation, false),
        "goal" | "fast" | "scripts" | "orchestrate" => at(AgentModes, false),
        "subagent-config" => at(Subagents, true),
        "subagents" => at(Subagents, false),
        n if n.starts_with("subagent-") => at(Subagents, false),
        n if n.starts_with("skill:") => at(Skills, false),
        "provider" | "login" | "logout" | "web-search-model" | "mcp" => at(SettingsShortcuts, true),
        n if n.starts_with("mcp-")
            || n.starts_with("pi-mcp")
            || n.starts_with("newapi-")
            || n.starts_with("llama") =>
        {
            at(SettingsShortcuts, true)
        }
        n if n.ends_with("-config") => at(Other, true),
        _ => at(Other, false),
    }
}

/// The glyph for a command: what it acts on, matching Settings' own icons
/// where a command opens or repeats one of its pages (a key for providers, a
/// globe for MCP). Unknown commands keep the generic command glyph, or the
/// sliders when their name says they configure something.
pub fn icon(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "compact" => icons::FOLD_VERTICAL,
        "export-html" => icons::ARCHIVE_UP_MINIMALISTIC,
        "goal" => icons::FLAG,
        "fast" => icons::BOLT,
        "scripts" => icons::CODE,
        "orchestrate" => icons::HIERARCHY,
        "subagents" => icons::USERS,
        "subagent-status" => icons::PULSE,
        "subagent-retry" => icons::RESTART,
        "provider" | "login" => icons::KEY_MINIMALISTIC,
        "logout" => icons::LOGOUT_2,
        "web-search-model" => icons::MAGNIFER,
        "mcp" => icons::GLOBAL,
        n if n.starts_with("skill:") => icons::BOOK,
        n if n.starts_with("mcp-") || n.starts_with("pi-mcp") => icons::GLOBAL,
        n if n.starts_with("newapi-") => icons::CLOUD,
        n if n.starts_with("llama") => icons::LAPTOP,
        n if n.ends_with("-config") => icons::TUNING,
        _ => icons::COMMAND,
    }
}

/// Which Providers page view a `/provider`, `/login` or `/logout` command
/// typed in the composer opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderIntent {
    List,
    Add,
    Edit(String),
    Logout(String),
}

pub fn command_intent(text: &str) -> Option<ProviderIntent> {
    let parts: Vec<_> = text.split_whitespace().collect();
    match parts.as_slice() {
        ["/provider"] | ["/login"] | ["/logout"] => Some(ProviderIntent::List),
        ["/provider", "add"] => Some(ProviderIntent::Add),
        ["/login", id, ..] => Some(ProviderIntent::Edit((*id).into())),
        ["/logout", id] => Some(ProviderIntent::Logout((*id).into())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn nothing_is_shown_until_turned_on() {
        for name in [
            "compact",
            "goal",
            "subagents",
            "mcp",
            "skill:x",
            "brand-new",
        ] {
            assert!(!shows(&[], name), "{name}");
        }
    }

    #[test]
    fn turning_on_and_off_keeps_order_and_other_devices_names() {
        let shown = set_visible(&[], &names(&["goal", "fast"]), true);
        assert_eq!(shown, names(&["goal", "fast"]));
        // Turning on twice does not duplicate.
        let shown = set_visible(&shown, &names(&["goal", "compact"]), true);
        assert_eq!(shown, names(&["goal", "fast", "compact"]));
        let shown = set_visible(&shown, &names(&["fast"]), false);
        assert_eq!(shown, names(&["goal", "compact"]));
        // A name this device does not offer stays in the list.
        let shown = set_visible(&names(&["remote-only"]), &names(&["goal"]), true);
        assert!(shows(&shown, "remote-only"));
    }

    #[test]
    fn defaults_are_turned_on_once() {
        let (mut shown, mut offered) = (names(&["goal"]), Vec::new());
        offer_defaults(&mut shown, &mut offered);
        // Already on: not listed twice.
        assert_eq!(
            shown,
            names(&[
                "goal",
                "compact",
                "fast",
                "scripts",
                "orchestrate",
                "subagent-status"
            ])
        );
        assert_eq!(offered, names(SHOWN_BY_DEFAULT));
        // Turned off afterwards: the next load leaves it off.
        let mut shown = set_visible(&shown, &names(&["scripts"]), false);
        offer_defaults(&mut shown, &mut offered);
        assert!(!shows(&shown, "scripts"));
        assert_eq!(offered, names(SHOWN_BY_DEFAULT));
    }

    #[test]
    fn later_defaults_reach_existing_installs() {
        // Offered `scripts` alone, and turned it off since.
        let (mut shown, mut offered) = (Vec::new(), names(&["scripts"]));
        offer_defaults(&mut shown, &mut offered);
        assert_eq!(
            shown,
            names(&["compact", "goal", "fast", "orchestrate", "subagent-status"])
        );
    }

    #[test]
    fn commands_get_icons_for_what_they_act_on() {
        let cases = [
            ("compact", icons::FOLD_VERTICAL),
            ("export-html", icons::ARCHIVE_UP_MINIMALISTIC),
            ("goal", icons::FLAG),
            ("fast", icons::BOLT),
            ("scripts", icons::CODE),
            ("orchestrate", icons::HIERARCHY),
            ("subagents", icons::USERS),
            ("subagent-status", icons::PULSE),
            ("subagent-retry", icons::RESTART),
            ("subagent-config", icons::TUNING),
            ("skill:collabmd", icons::BOOK),
            ("provider", icons::KEY_MINIMALISTIC),
            ("login", icons::KEY_MINIMALISTIC),
            ("logout", icons::LOGOUT_2),
            ("web-search-model", icons::MAGNIFER),
            ("mcp", icons::GLOBAL),
            ("newapi-provider-add", icons::CLOUD),
            ("llama", icons::LAPTOP),
            ("compact-ui-config", icons::TUNING),
            ("my-prompt", icons::COMMAND),
        ];
        for (name, icon) in cases {
            assert_eq!(super::icon(name), icon, "{name}");
        }
    }

    #[test]
    fn commands_land_in_their_groups() {
        use CommandGroup::*;
        let cases = [
            ("compact", Conversation, false),
            ("export-html", Conversation, false),
            ("goal", AgentModes, false),
            ("fast", AgentModes, false),
            ("scripts", AgentModes, false),
            ("orchestrate", AgentModes, false),
            ("subagents", Subagents, false),
            ("subagent-status", Subagents, false),
            ("subagent-retry", Subagents, false),
            ("subagent-config", Subagents, true),
            ("skill:collabmd", Skills, false),
            ("provider", SettingsShortcuts, true),
            ("login", SettingsShortcuts, true),
            ("logout", SettingsShortcuts, true),
            ("web-search-model", SettingsShortcuts, true),
            ("mcp", SettingsShortcuts, true),
            ("mcp-auth", SettingsShortcuts, true),
            ("newapi-provider-add", SettingsShortcuts, true),
            ("newapi-config-recover", SettingsShortcuts, true),
            ("llama", SettingsShortcuts, true),
            // Unknown extensions: configuration by name, else Other.
            ("compact-ui-config", Other, true),
            ("perm-mode", Other, false),
            ("my-prompt", Other, false),
        ];
        for (name, group, advanced) in cases {
            assert_eq!(placement(name), Placement { group, advanced }, "{name}");
        }
    }

    #[test]
    fn only_exact_management_commands_open_settings() {
        assert_eq!(command_intent("/provider add"), Some(ProviderIntent::Add));
        assert_eq!(
            command_intent("/login mvp-lab"),
            Some(ProviderIntent::Edit("mvp-lab".into()))
        );
        assert_eq!(
            command_intent("/logout mvp-lab"),
            Some(ProviderIntent::Logout("mvp-lab".into()))
        );
        assert_eq!(
            command_intent("/login x secret"),
            Some(ProviderIntent::Edit("x".into()))
        );
        assert_eq!(command_intent("explain /provider"), None);
        assert_eq!(command_intent("/newapi-provider-add"), None);
    }
}
