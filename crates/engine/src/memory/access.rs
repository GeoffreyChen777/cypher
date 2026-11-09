//! Which stores a chat sees (docs/design/memory.md §1).
//!
//! - A session (and its subagents) sees **/you** and its project's
//!   **/project**; a subagent whose profile keeps memory also sees **/role**.
//! - A workspace's own threads see **/you**, **/workspace** and, read-only,
//!   **/projects/<name>** for each project in its scope: what the project's
//!   sessions learned. What the workspace learns about a project goes into
//!   its own store (`/workspace/projects/<name>.md`), never the project's.
//!   A routine thread writes the workspace store only when its routine may.
//! - A consolidation pass sees the one store it repairs.
//! - Another user's workspace asking (a peer thread) sees nothing.
//!
//! Resolved per call from the current rows, like the bot tools.

use std::collections::HashSet;

use cypher_proto::{
    self as p, Bot, BotOrigin, BotThread, Chat, MemoryAuthor, MemoryScope, RoleMemory, Space,
};

use super::Memory;
use crate::bots::prompt_space_name;

/// One store as a run sees it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Root {
    /// `/you`, `/project`, `/projects/billing`…
    pub path: String,
    pub scope: MemoryScope,
    pub write: bool,
    /// What it is, for prompts and listings.
    pub title: String,
}

impl Root {
    /// The app's view of a store: the owner may write any.
    pub(super) fn owner(scope: MemoryScope) -> Self {
        Self {
            path: String::new(),
            title: String::new(),
            scope,
            write: true,
        }
    }
}

/// Who is reading, and what they see.
#[derive(Debug, Clone)]
pub(super) struct Reader {
    pub chat: Chat,
    pub roots: Vec<Root>,
    pub author: MemoryAuthor,
    /// The workspace a workspace thread belongs to.
    pub bot: Option<Bot>,
    /// What `search_history` reads.
    pub history: History,
    /// The `memory` tool call being answered: a held write's card is drawn
    /// there.
    pub tool_call_id: Option<String>,
}

/// Which past conversations a reader may search and read.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum History {
    None,
    /// Sessions in one project.
    Project(String),
    /// A workspace's own chat and threads, and the sessions it may read.
    Workspace(String),
    /// Every session (the You store's pass).
    All,
    /// Sessions of one subagent profile (optionally in one project).
    Role {
        agent: String,
        space_id: Option<String>,
    },
}

impl Reader {
    /// The owner, editing in the app.
    pub(super) fn owner() -> Self {
        Self::accepting(MemoryAuthor::Owner)
    }

    /// Writing as `author` on the owner's word (an accepted held write).
    pub(super) fn accepting(author: MemoryAuthor) -> Self {
        Self {
            chat: owner_chat(),
            roots: Vec::new(),
            author,
            bot: None,
            history: History::None,
            tool_call_id: None,
        }
    }

    /// The root a path like `/project/notes.md` names, and the rest
    /// (`/notes.md`, or empty for the root itself). Longest root first, so
    /// `/projects/x` wins over a would-be `/projects`.
    pub(super) fn split<'a>(&'a self, path: &str) -> Option<(&'a Root, String)> {
        let path = path.trim();
        let mut roots: Vec<&Root> = self.roots.iter().collect();
        roots.sort_by_key(|r| std::cmp::Reverse(r.path.len()));
        roots.into_iter().find_map(|root| {
            let rest = path.strip_prefix(root.path.as_str())?;
            (rest.is_empty() || rest == "/" || rest.starts_with('/'))
                .then(|| (root, rest.trim_end_matches('/').to_string()))
        })
    }
}

/// A placeholder chat for owner edits (never stored or shown).
fn owner_chat() -> Chat {
    Chat {
        id: String::new(),
        device_id: String::new(),
        title: None,
        archived: false,
        pinned: false,
        cwd: None,
        branch: None,
        checkout_id: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: chrono::DateTime::from_timestamp_millis(0).unwrap_or_default(),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        child: None,
        bot: None,
        memory: None,
    }
}

/// The answer to writing a read-only root.
pub(super) fn read_only_hint(reader: &Reader, root: &Root) -> String {
    match &root.scope {
        MemoryScope::Project { .. } if reader.bot.is_some() => {
            let name = root.path.rsplit('/').next().unwrap_or("project");
            format!(
                "{} is the project's own memory, written by its sessions; you read it but don't \
                 change it. Keep what you learn about this project in /workspace/projects/{name}.md.",
                root.path
            )
        }
        _ => format!("{} is read-only in this session.", root.path),
    }
}

/// A short, path-safe name for a project: its display name, lower case,
/// with anything else as `-`.
pub(super) fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out
        .trim_end_matches('-')
        .chars()
        .take(40)
        .collect::<String>();
    if out.is_empty() {
        "project".into()
    } else {
        out
    }
}

/// `(space, slug)` for the spaces given, slugs made unique.
pub(super) fn slugs<'a>(spaces: &[&'a Space]) -> Vec<(&'a Space, String)> {
    let mut seen = HashSet::new();
    spaces
        .iter()
        .map(|space| {
            let base = slug(&prompt_space_name(space));
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}-{n}");
                n += 1;
            }
            (*space, name)
        })
        .collect()
}

impl Memory {
    /// The reader for `chat_id`: `None` for a chat that has no memory (a
    /// peer thread, or a chat this engine doesn't know).
    pub(super) fn reader(&self, chat_id: &str) -> Option<Reader> {
        let workspace = &self.inner.workspace;
        let chat = workspace.chat(chat_id).ok().flatten()?;
        let spaces = workspace.read_spaces().unwrap_or_default();
        let space_title = |id: &str| {
            spaces
                .iter()
                .find(|s| s.id == id)
                .map(prompt_space_name)
                .unwrap_or_else(|| "this project".into())
        };
        let you = |write: bool| Root {
            path: "/you".into(),
            scope: MemoryScope::You,
            write,
            title: "your memory of the user, across every project".into(),
        };
        let title = chat.title.clone().filter(|t| !t.trim().is_empty());

        if let Some(pass) = &chat.memory {
            let root = match &pass.scope {
                MemoryScope::You => you(true),
                MemoryScope::Project { space_id } => Root {
                    path: "/project".into(),
                    scope: pass.scope.clone(),
                    write: true,
                    title: format!("the memory of the project “{}”", space_title(space_id)),
                },
                MemoryScope::Workspace { .. } => Root {
                    path: "/workspace".into(),
                    scope: pass.scope.clone(),
                    write: true,
                    title: "the workspace's memory".into(),
                },
                MemoryScope::Role { agent, .. } => Root {
                    path: "/role".into(),
                    scope: pass.scope.clone(),
                    write: true,
                    title: format!("the memory of the “{agent}” agent"),
                },
            };
            let history = match &pass.scope {
                MemoryScope::You => History::All,
                MemoryScope::Project { space_id } => History::Project(space_id.clone()),
                MemoryScope::Workspace { bot_id } => History::Workspace(bot_id.clone()),
                MemoryScope::Role { agent, space_id } => History::Role {
                    agent: agent.clone(),
                    space_id: space_id.clone(),
                },
            };
            let bot = match &pass.scope {
                MemoryScope::Workspace { bot_id } => workspace.bot(bot_id).ok().flatten(),
                _ => None,
            };
            return Some(Reader {
                author: MemoryAuthor::Consolidation {
                    pass_id: pass.pass_id.clone(),
                },
                roots: vec![root],
                chat,
                bot,
                history,
                tool_call_id: None,
            });
        }

        if let Some(marker) = chat.bot.clone().filter(|m| m.is_bot_thread()) {
            if marker.origin == BotOrigin::Peer {
                return None;
            }
            let bot = workspace.bot(&marker.bot_id).ok().flatten()?;
            let owner = marker.origin == BotOrigin::Owner;
            let writes = owner || self.routine_writes(&marker);
            let mut roots = vec![
                you(owner),
                Root {
                    path: "/workspace".into(),
                    scope: MemoryScope::Workspace {
                        bot_id: bot.id.clone(),
                    },
                    write: writes,
                    title: format!("“{}”'s own memory: what this workspace learned", bot.name),
                },
            ];
            let visible: Vec<&Space> = spaces
                .iter()
                .filter(|s| bot.scope.sees_space(&s.id))
                .collect();
            for (space, name) in slugs(&visible) {
                roots.push(Root {
                    path: format!("/projects/{name}"),
                    scope: MemoryScope::Project {
                        space_id: space.id.clone(),
                    },
                    write: false,
                    title: format!(
                        "the project “{}”'s memory, written by its sessions (read-only)",
                        prompt_space_name(space)
                    ),
                });
            }
            return Some(Reader {
                author: MemoryAuthor::Workspace {
                    bot_id: bot.id.clone(),
                    chat_id: chat.id.clone(),
                    title,
                },
                roots,
                history: History::Workspace(bot.id.clone()),
                bot: Some(bot),
                chat,
                tool_call_id: None,
            });
        }

        // A session: a plain one, a subagent's, or a project routine's.
        let mut roots = vec![you(true)];
        let mut history = History::None;
        if let Some(space_id) = chat.space_id.clone() {
            roots.push(Root {
                path: "/project".into(),
                title: format!(
                    "the memory of the project “{}”, shared by its sessions",
                    space_title(&space_id)
                ),
                scope: MemoryScope::Project {
                    space_id: space_id.clone(),
                },
                write: true,
            });
            history = History::Project(space_id);
        }
        if let Some(child) = &chat.child
            && let Some(memory) = self.role_memory(&child.agent)
        {
            let space_id = match memory {
                RoleMemory::User => None,
                RoleMemory::Project => chat.space_id.clone(),
            };
            // A per-project agent outside any project has no store.
            if memory == RoleMemory::User || space_id.is_some() {
                roots.push(Root {
                    path: "/role".into(),
                    title: format!("the “{}” agent's own memory", child.agent),
                    scope: MemoryScope::Role {
                        agent: child.agent.clone(),
                        space_id,
                    },
                    write: true,
                });
            }
        }
        Some(Reader {
            author: MemoryAuthor::Session {
                chat_id: chat.id.clone(),
                title,
            },
            roots,
            history,
            bot: None,
            chat,
            tool_call_id: None,
        })
    }

    /// Whether a routine thread's routine may write the workspace's memory.
    fn routine_writes(&self, marker: &p::BotChat) -> bool {
        let BotThread::Routine { routine_id } = &marker.thread else {
            return false;
        };
        self.inner
            .workspace
            .record::<p::BotRoutine>(p::KIND_BOT_ROUTINES, routine_id)
            .is_some_and(|r| r.bot_id == marker.bot_id && r.write_notes)
    }

    /// A subagent profile's `memory:`, read from the installed profiles.
    fn role_memory(&self, agent: &str) -> Option<RoleMemory> {
        let paths = lock_clone(&self.inner.pi_runtime)?;
        crate::pi::subagents::list(&paths)
            .into_iter()
            .find(|a| a.name == agent)
            .and_then(|a| a.memory)
    }

    /// The chats `history` reads, newest activity first; never peer
    /// threads or passes.
    pub(super) fn history_chats(&self, history: &History) -> Vec<Chat> {
        let workspace = &self.inner.workspace;
        let chats = workspace.read_chats().unwrap_or_default();
        let session = |c: &&Chat| {
            c.memory.is_none()
                && c.bot
                    .as_ref()
                    .is_none_or(|m| matches!(m.thread, BotThread::RoutineSession { .. }))
        };
        let mut out: Vec<Chat> = match history {
            History::None => Vec::new(),
            History::Project(space_id) => chats
                .iter()
                .filter(session)
                .filter(|c| c.space_id.as_deref() == Some(space_id))
                .cloned()
                .collect(),
            History::All => chats.iter().filter(session).cloned().collect(),
            History::Role { agent, space_id } => chats
                .iter()
                .filter(session)
                .filter(|c| c.child.as_ref().is_some_and(|child| &child.agent == agent))
                .filter(|c| space_id.is_none() || c.space_id == *space_id)
                .cloned()
                .collect(),
            History::Workspace(bot_id) => {
                let Some(bot) = workspace.bot(bot_id).ok().flatten() else {
                    return Vec::new();
                };
                let mut own: Vec<Chat> = chats
                    .iter()
                    .filter(|c| c.memory.is_none())
                    .filter(|c| {
                        c.bot.as_ref().is_some_and(|m| {
                            m.bot_id == bot.id && m.origin != BotOrigin::Peer && m.is_bot_thread()
                        })
                    })
                    .cloned()
                    .collect();
                if bot.scope.transcripts {
                    own.extend(bot.scope.visible_chats(&chats).into_iter().cloned());
                }
                own
            }
        };
        let activity = |c: &Chat| c.last_message_at.unwrap_or(c.created_at);
        out.sort_by(|a, b| activity(b).cmp(&activity(a)).then(a.id.cmp(&b.id)));
        out.dedup_by(|a, b| a.id == b.id);
        out
    }
}

fn lock_clone<T: Clone>(slot: &std::sync::Mutex<Option<T>>) -> Option<T> {
    crate::util::lock(slot).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_short_unique_and_path_safe() {
        assert_eq!(slug("Invoice Demo"), "invoice-demo");
        assert_eq!(slug("  cypher/main!! "), "cypher-main");
        assert_eq!(slug("···"), "project");
        let space = |id: &str, path: &str| Space {
            id: id.into(),
            device_id: "dev".into(),
            path: path.into(),
            name: None,
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: chrono::Utc::now(),
            pinned: false,
            icon: None,
            color: None,
        };
        let a = space("a", "/x/api");
        let b = space("b", "/y/api");
        let named = slugs(&[&a, &b]);
        assert_eq!(named[0].1, "api");
        assert_eq!(named[1].1, "api-2");
    }

    #[test]
    fn a_path_splits_at_its_longest_root() {
        let reader = Reader {
            roots: vec![
                Root {
                    path: "/workspace".into(),
                    scope: MemoryScope::Workspace { bot_id: "b".into() },
                    write: true,
                    title: String::new(),
                },
                Root {
                    path: "/projects/api".into(),
                    scope: MemoryScope::Project {
                        space_id: "s".into(),
                    },
                    write: false,
                    title: String::new(),
                },
            ],
            ..Reader::owner()
        };
        let (root, rest) = reader.split("/projects/api/MEMORY.md").unwrap();
        assert_eq!(root.path, "/projects/api");
        assert_eq!(rest, "/MEMORY.md");
        let (root, rest) = reader.split("/workspace").unwrap();
        assert_eq!(root.path, "/workspace");
        assert_eq!(rest, "");
        assert!(reader.split("/workspaces/x.md").is_none());
        assert!(reader.split("/you/MEMORY.md").is_none());
    }
}
