//! A workspace chat's file roots: the folders its Files and Git surfaces
//! may open (`ListChatFileRoots`, docs/features/files.md "Workspace
//! chats"). The workspace's own folder comes first (the chat's cwd), then
//! each project its scope grants on the workspace's host, sorted by name.
//!
//! A session has one root, its checkout, and keeps the single-root code
//! paths; everything here is for a workspace's own chat. Pure: the state
//! holds the list ([`crate::state::AppState::file_roots`]), the surfaces
//! read it through these rules.

use cypher_proto::{Bot, Chat, ChatFileRoot};

use super::links;

/// Which root: a granted project's space id, `None` for the workspace's
/// own folder (the default root).
pub type RootId = Option<String>;

/// Whether `chat` is a workspace's own chat: the owner's Main thread that
/// its workspace names as its chat. Hidden threads (plans, routines, peer
/// answers), memory passes and a marker whose workspace is gone are not.
pub fn is_workspace_chat(chat: &Chat, bots: &[Bot]) -> bool {
    let Some(marker) = chat.bot.as_ref().filter(|marker| marker.is_owner_chat()) else {
        return false;
    };
    !chat.is_memory_pass()
        && bots
            .iter()
            .any(|bot| bot.id == marker.bot_id && bot.chat_id == chat.id)
}

/// The list an older engine (no `ListChatFileRoots`) stands for: the
/// workspace's own folder only. Empty when the chat has no folder.
pub fn fallback_roots(chat: &Chat, name: &str) -> Vec<ChatFileRoot> {
    chat.cwd
        .iter()
        .map(|cwd| ChatFileRoot {
            space_id: None,
            name: name.to_owned(),
            path: cwd.clone(),
            checkout_id: None,
        })
        .collect()
}

/// The root a surface shows: the picked one while it is still listed (and
/// `eligible`), else the first granted project, else the first root.
/// `eligible` narrows the choice (Git: checkouts only).
pub fn pick<'a>(
    roots: &'a [ChatFileRoot],
    picked: Option<&RootId>,
    eligible: impl Fn(&ChatFileRoot) -> bool,
) -> Option<&'a ChatFileRoot> {
    if let Some(picked) = picked
        && let Some(root) = roots
            .iter()
            .find(|root| root.space_id == *picked && eligible(root))
    {
        return Some(root);
    }
    roots
        .iter()
        .find(|root| root.space_id.is_some() && eligible(root))
        .or_else(|| roots.iter().find(|root| eligible(root)))
}

/// The root an absolute `path` lies in (the innermost one when roots
/// nest), with the path relative to it (`""` for the root itself). `None`
/// outside every root.
pub fn containing<'a>(roots: &'a [ChatFileRoot], path: &str) -> Option<(&'a ChatFileRoot, String)> {
    roots
        .iter()
        .filter_map(|root| {
            links::resolve(path, &root.path)
                .ok()
                .map(|relative| (root, relative))
        })
        .max_by_key(|(root, _)| root.path.trim_end_matches('/').len())
}

/// Where a chat link's path may be, in the order to try: an absolute path
/// only in the root it lies in; a relative one in the chat's own folder
/// first, then each granted project, in list order (a `../` that climbs
/// out of a root skips it). Indices into `roots`, with the path relative to
/// that root.
pub fn candidates(roots: &[ChatFileRoot], path: &str) -> Vec<(usize, String)> {
    if path.starts_with('/') {
        return containing(roots, path)
            .and_then(|(root, relative)| {
                roots
                    .iter()
                    .position(|r| std::ptr::eq(r, root))
                    .map(|ix| (ix, relative))
            })
            .into_iter()
            .collect();
    }
    roots
        .iter()
        .enumerate()
        .filter_map(|(ix, root)| {
            links::resolve(path, &root.path)
                .ok()
                .map(|relative| (ix, relative))
        })
        .collect()
}

/// `relative` inside `root` as one absolute path: how a workspace chat's
/// resolved reference travels to the link handler, which finds the root
/// again with [`containing`].
pub fn join(root: &ChatFileRoot, relative: &str) -> String {
    let base = root.path.trim_end_matches('/');
    if relative.is_empty() {
        base.to_owned()
    } else {
        format!("{base}/{relative}")
    }
}

/// How a path inside a root reads to the owner: the root's name, then the
/// path in it (`cypher/src/a.rs`). The name alone for the root itself.
pub fn display(root: &ChatFileRoot, relative: &str) -> String {
    if relative.is_empty() {
        root.name.clone()
    } else {
        format!("{}/{relative}", root.name)
    }
}

/// What `ListChatFileRoots` was asked for: the chat, its host, and what
/// decides the granted projects (the scope and the host's projects), so a
/// scope edit or a project added on the host lists again. `None` when the
/// chat is not a workspace's own chat.
pub fn list_key(chat: &Chat, bots: &[Bot], spaces: &[cypher_proto::Space]) -> Option<String> {
    if !is_workspace_chat(chat, bots) {
        return None;
    }
    let bot = bots.iter().find(|bot| bot.chat_id == chat.id)?;
    let mut granted: Vec<String> = spaces
        .iter()
        .filter(|space| space.device_id == bot.host_device_id && bot.scope.sees_space(&space.id))
        .map(|space| {
            format!(
                "{}={}@{}#{}",
                space.id,
                space.display_name(),
                space.path,
                space.checkout_id.as_deref().unwrap_or_default()
            )
        })
        .collect();
    granted.sort();
    Some(format!(
        "{}|{}|{}|{}|{}",
        chat.id,
        chat.device_id,
        chat.cwd.as_deref().unwrap_or_default(),
        bot.name,
        granted.join(",")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures;
    use cypher_proto::BotChat;

    fn root(space: Option<&str>, name: &str, path: &str) -> ChatFileRoot {
        ChatFileRoot {
            space_id: space.map(str::to_owned),
            name: name.into(),
            path: path.into(),
            checkout_id: space.map(|id| format!("co-{id}")),
        }
    }

    fn roots() -> Vec<ChatFileRoot> {
        vec![
            root(None, "Billing", "/data/bots/b"),
            root(Some("api"), "api", "/src/api"),
            root(Some("web"), "web", "/src/web"),
        ]
    }

    #[test]
    fn only_a_workspaces_own_chat_has_roots() {
        let bots = vec![test_fixtures::bot()];
        let own = Chat {
            id: "bot-chat".into(),
            bot: Some(BotChat::owner("b")),
            ..test_fixtures::chat()
        };
        assert!(is_workspace_chat(&own, &bots));
        // A hidden thread of the same workspace.
        let thread = Chat {
            bot: Some(BotChat::thread(
                "b",
                cypher_proto::BotOrigin::Owner,
                cypher_proto::BotThread::Work {
                    work_id: "w".into(),
                },
            )),
            ..own.clone()
        };
        assert!(!is_workspace_chat(&thread, &bots));
        // A routine's thread is not the owner's.
        let routine = Chat {
            bot: Some(BotChat::new("b", cypher_proto::BotOrigin::Routine)),
            ..own.clone()
        };
        assert!(!is_workspace_chat(&routine, &bots));
        // A marker naming a chat the workspace does not call its own.
        let other = Chat {
            id: "elsewhere".into(),
            ..own.clone()
        };
        assert!(!is_workspace_chat(&other, &bots));
        // The workspace is gone.
        assert!(!is_workspace_chat(&own, &[]));
        // A plain session.
        assert!(!is_workspace_chat(&test_fixtures::chat(), &bots));
    }

    #[test]
    fn the_shown_root_is_the_pick_else_the_first_project() {
        let roots = roots();
        let any = |_: &ChatFileRoot| true;
        // Nothing picked: the first granted project, not the workspace's
        // own folder.
        assert_eq!(pick(&roots, None, any).unwrap().name, "api");
        // A pick (the picker, or the last opened reference) wins.
        let web = Some("web".to_string());
        assert_eq!(pick(&roots, Some(&web), any).unwrap().name, "web");
        assert_eq!(pick(&roots, Some(&None), any).unwrap().name, "Billing");
        // A pick no longer listed (the grant was withdrawn) falls back.
        let gone = Some("gone".to_string());
        assert_eq!(pick(&roots, Some(&gone), any).unwrap().name, "api");
        // No granted project: the workspace's folder.
        assert_eq!(pick(&roots[..1], None, any).unwrap().name, "Billing");
        assert!(pick(&[], None, any).is_none());
    }

    #[test]
    fn git_picks_only_checkouts() {
        let mut roots = roots();
        roots[1].checkout_id = None;
        let git = |root: &ChatFileRoot| root.checkout_id.is_some();
        // "api" is not a checkout: the first one that is.
        assert_eq!(pick(&roots, None, git).unwrap().name, "web");
        // Picking the workspace's folder in Files leaves Git on a checkout.
        assert_eq!(pick(&roots, Some(&None), git).unwrap().name, "web");
        assert!(pick(&roots[..2], None, git).is_none());
    }

    #[test]
    fn an_older_engine_stands_for_the_workspace_folder_alone() {
        let chat = Chat {
            cwd: Some("/data/bots/b".into()),
            ..test_fixtures::chat()
        };
        let roots = fallback_roots(&chat, "Billing");
        assert_eq!(roots, vec![root(None, "Billing", "/data/bots/b")]);
        // The picker then has nothing to offer and Files shows that folder.
        assert_eq!(pick(&roots, None, |_| true).unwrap().space_id, None);
        assert!(fallback_roots(&test_fixtures::chat(), "Billing").is_empty());
    }

    #[test]
    fn relative_paths_try_the_chat_folder_first_then_each_project() {
        let roots = roots();
        assert_eq!(
            candidates(&roots, "src/main.rs"),
            vec![
                (0, "src/main.rs".to_string()),
                (1, "src/main.rs".to_string()),
                (2, "src/main.rs".to_string()),
            ]
        );
        // A `../` out of every root.
        assert!(candidates(&roots, "../../../etc/hosts").is_empty());
    }

    #[test]
    fn absolute_paths_open_only_inside_a_root() {
        let roots = roots();
        assert_eq!(
            candidates(&roots, "/src/web/app/page.tsx"),
            vec![(2, "app/page.tsx".to_string())]
        );
        assert_eq!(candidates(&roots, "/src/api"), vec![(1, String::new())]);
        assert_eq!(
            candidates(&roots, "/data/bots/b/notes.md"),
            vec![(0, "notes.md".to_string())]
        );
        // Outside every root (a sibling with a shared prefix included).
        assert!(candidates(&roots, "/etc/hosts").is_empty());
        assert!(candidates(&roots, "/src/website/a.rs").is_empty());
        assert!(candidates(&roots, "/src").is_empty());
    }

    #[test]
    fn nested_roots_resolve_to_the_innermost() {
        let roots = vec![
            root(None, "Billing", "/data/bots/b"),
            root(Some("mono"), "mono", "/src/mono"),
            root(Some("pkg"), "pkg", "/src/mono/pkg"),
        ];
        let (found, relative) = containing(&roots, "/src/mono/pkg/lib.rs").unwrap();
        assert_eq!((found.name.as_str(), relative.as_str()), ("pkg", "lib.rs"));
        let (found, relative) = containing(&roots, "/src/mono/README.md").unwrap();
        assert_eq!(
            (found.name.as_str(), relative.as_str()),
            ("mono", "README.md")
        );
    }

    #[test]
    fn a_resolved_reference_round_trips_through_its_absolute_path() {
        let roots = roots();
        let joined = join(&roots[2], "app/page.tsx");
        assert_eq!(joined, "/src/web/app/page.tsx");
        let (found, relative) = containing(&roots, &joined).unwrap();
        assert_eq!(found.space_id.as_deref(), Some("web"));
        assert_eq!(relative, "app/page.tsx");
        assert_eq!(display(found, &relative), "web/app/page.tsx");
        assert_eq!(join(&roots[1], ""), "/src/api");
    }

    #[test]
    fn the_list_key_follows_the_grant() {
        let bot = cypher_proto::Bot {
            scope: cypher_proto::BotScope {
                projects: cypher_proto::ProjectGrant::Selected {
                    space_ids: vec!["api".into()],
                },
                ..Default::default()
            },
            ..test_fixtures::bot()
        };
        let chat = Chat {
            id: "bot-chat".into(),
            bot: Some(BotChat::owner("b")),
            ..test_fixtures::chat()
        };
        let api = cypher_proto::Space {
            id: "api".into(),
            path: "/src/api".into(),
            ..test_fixtures::space()
        };
        let web = cypher_proto::Space {
            id: "web".into(),
            path: "/src/web".into(),
            ..test_fixtures::space()
        };
        let spaces = vec![api.clone(), web.clone()];
        let key = list_key(&chat, std::slice::from_ref(&bot), &spaces).unwrap();
        // An ungranted project changing does not list again.
        let moved_web = vec![
            api.clone(),
            cypher_proto::Space {
                path: "/elsewhere".into(),
                ..web.clone()
            },
        ];
        assert_eq!(
            list_key(&chat, std::slice::from_ref(&bot), &moved_web).unwrap(),
            key
        );
        // Granting another project does.
        let wider = cypher_proto::Bot {
            scope: cypher_proto::BotScope {
                projects: cypher_proto::ProjectGrant::All,
                ..Default::default()
            },
            ..bot.clone()
        };
        assert_ne!(list_key(&chat, &[wider], &spaces).unwrap(), key);
        // Not a workspace chat: nothing to list.
        assert!(list_key(&test_fixtures::chat(), &[bot], &spaces).is_none());
    }
}
