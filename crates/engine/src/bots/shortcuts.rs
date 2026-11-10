//! Shortcuts (docs/design/agent-bots.md §13.1): buttons at the bottom of
//! the workspace's sidebar, each a request the owner makes often. Only the
//! agent makes and changes them, in the workspace chat, when its owner asks
//! ("make a button for this"); a click sends the request back to it.

use serde::Deserialize;

use cypher_proto::{self as p, BotChat, BotOrigin, BotShortcut, Chat};

use super::Bots;
use crate::EngineError;
use crate::host::workspace_host::Fields;
use crate::util::new_id;

/// What the workspace chat is told about shortcuts.
const SHORTCUTS_LINE: &str = "Shortcuts:\n- Shortcuts are buttons at the bottom of \
     your owner's sidebar. A click sends its request to you here, as if they had typed it. \
     When your owner asks for a button (\"make this a shortcut\", \"add a button that…\"), save \
     one with save_shortcut: a short label, an icon, and the request in their words, with \
     {input} where each click should ask them for the part that changes. When they ask for \
     the same kind of thing again and again, offer one. list_shortcuts shows them; rename, \
     change or remove one when asked.";

/// This track's section of the thread's prompt: the workspace chat's only.
pub(super) fn shortcuts_prompt(marker: &BotChat) -> Option<String> {
    (!shortcut_tools(marker).is_empty()).then(|| SHORTCUTS_LINE.to_string())
}

/// The tools of this track a thread may call: the workspace chat's only.
pub(super) fn shortcut_tools(marker: &BotChat) -> Vec<&'static str> {
    if marker.origin == BotOrigin::Owner && marker.is_main() {
        p::BOT_SHORTCUT_TOOLS.to_vec()
    } else {
        Vec::new()
    }
}

#[derive(Debug, Deserialize)]
struct SaveShortcut {
    #[serde(default)]
    id: Option<String>,
    label: String,
    #[serde(default)]
    icon: Option<String>,
    request: String,
}

#[derive(Debug, Deserialize)]
struct DeleteShortcut {
    id: String,
}

fn args<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Result<T, EngineError> {
    serde_json::from_value(value.clone())
        .map_err(|err| EngineError::Other(format!("Invalid arguments: {err}")))
}

impl Bots {
    /// Answer `tool` when it is one of this track's; `None` otherwise.
    pub(super) fn shortcut_tool(
        &self,
        chat: &Chat,
        marker: &BotChat,
        tool: &str,
        raw: &serde_json::Value,
    ) -> Option<Result<String, EngineError>> {
        if !p::BOT_SHORTCUT_TOOLS.contains(&tool) {
            return None;
        }
        Some(self.shortcut_call(chat, marker, tool, raw))
    }

    fn shortcut_call(
        &self,
        chat: &Chat,
        marker: &BotChat,
        tool: &str,
        raw: &serde_json::Value,
    ) -> Result<String, EngineError> {
        let workspace = &self.inner.workspace;
        let bot = workspace
            .bot(&marker.bot_id)?
            .filter(|bot| bot.chat_id == chat.id)
            .ok_or_else(|| {
                EngineError::Other("Shortcuts are made in the workspace chat.".into())
            })?;
        if shortcut_tools(marker).is_empty() {
            return Err(EngineError::Other(
                "Shortcuts are made in the workspace chat, when your owner asks.".into(),
            ));
        }
        let records = workspace.read_bot_records();
        let mine = records.shortcuts_of(&bot.id);
        match tool {
            "list_shortcuts" => Ok(if mine.is_empty() {
                "No shortcuts yet.".into()
            } else {
                let mut out = vec!["Shortcuts, in the order they show:".to_string()];
                out.extend(mine.iter().map(|s| {
                    format!(
                        "- \"{}\" (id {}, icon {}): {}",
                        s.label,
                        s.id,
                        s.icon.as_deref().unwrap_or("none"),
                        s.request.split_whitespace().collect::<Vec<_>>().join(" ")
                    )
                }));
                out.join("\n")
            }),
            "save_shortcut" => {
                let a: SaveShortcut = args(raw)?;
                let now = p::now_ms();
                let existing = match a.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    Some(id) => Some(
                        mine.iter()
                            .find(|s| s.id == id)
                            .map(|s| (*s).clone())
                            .ok_or_else(|| {
                                EngineError::Other(format!(
                                    "No shortcut {id}. list_shortcuts shows them."
                                ))
                            })?,
                    ),
                    None => None,
                };
                let mut shortcut = BotShortcut {
                    id: existing.as_ref().map_or_else(new_id, |s| s.id.clone()),
                    bot_id: bot.id.clone(),
                    label: a.label,
                    icon: a.icon.map(|i| i.trim().to_lowercase()),
                    request: a.request,
                    order: existing.as_ref().map_or_else(
                        || mine.iter().map(|s| s.order + 1).max().unwrap_or(0),
                        |s| s.order,
                    ),
                    created_at: existing.as_ref().map_or(now, |s| s.created_at),
                    updated_at: now,
                };
                shortcut.clamp();
                shortcut.validate().map_err(EngineError::Other)?;
                workspace.put_record(
                    p::KIND_BOT_SHORTCUTS,
                    &shortcut.id,
                    &shortcut,
                    Fields::All,
                )?;
                Ok(format!(
                    "{} the shortcut \"{}\" (id {}){}. It is at the bottom of your owner's \
                     sidebar; a click sends: {}",
                    if existing.is_some() {
                        "Updated"
                    } else {
                        "Added"
                    },
                    shortcut.label,
                    shortcut.id,
                    if shortcut.wants_input() {
                        "; a click asks for the {input} part first"
                    } else {
                        ""
                    },
                    shortcut.request
                ))
            }
            "delete_shortcut" => {
                let a: DeleteShortcut = args(raw)?;
                let shortcut = mine.iter().find(|s| s.id == a.id.trim()).ok_or_else(|| {
                    EngineError::Other(format!("No shortcut {}. list_shortcuts shows them.", a.id))
                })?;
                workspace.delete_records(p::KIND_BOT_SHORTCUTS, std::slice::from_ref(&shortcut.id));
                Ok(format!("Removed the shortcut \"{}\".", shortcut.label))
            }
            _ => Err(EngineError::Other(format!("{tool} isn't available."))),
        }
    }
}
