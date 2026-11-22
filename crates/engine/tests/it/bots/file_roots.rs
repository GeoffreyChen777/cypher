//! A workspace's own chat opening files and git state beyond its private
//! folder: the projects its scope grants on its host (`ListChatFileRoots`,
//! the file RPCs' `rootSpaceId`, the diff watch), and nothing for anyone
//! else.

use crate::support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use cypher_engine::EngineCore;
use cypher_proto::{
    Bot, BotChat, BotOrigin, BotScope, BotThread, Chat, ChatFileRoot, MemoryChat, MemoryScope,
    ProjectGrant,
};
use cypher_rpc::{RpcClient, methods};

/// Projects: `granted` (git, with a dirty file), `secret` (git, not granted),
/// and `remote` (granted, but another device's). No project has a session.
struct Rig {
    core: EngineCore,
    client: RpcClient,
    bot: Bot,
    /// The workspace chat's own `cwd`, as its row has it.
    cwd: String,
    granted: PathBuf,
    secret: PathBuf,
    _tmp: tempfile::TempDir,
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let core = support::assemble_mock(&tmp.path().join("data"));
    let client = cypher_rpc::memory_client(core.rpc_service());
    let granted = tmp.path().join("granted");
    let secret = tmp.path().join("secret");
    for (id, dir) in [("sp-granted", &granted), ("sp-secret", &secret)] {
        support::init_repo(dir, "one\n").await;
        core.workspace
            .create_space(id, &core.device_id, dir.to_str().unwrap(), None, true)
            .unwrap();
    }
    std::fs::write(granted.join("a.txt"), "one\nedited\n").unwrap();
    std::fs::write(granted.join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(secret.join("plans.txt"), "secret plans\n").unwrap();
    core.workspace
        .create_space(
            "sp-remote",
            "other-device",
            granted.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    let value = client
        .call(
            methods::CREATE_BOT,
            serde_json::json!({
                "targetDeviceId": core.device_id,
                "name": "Billing",
                "scope": BotScope {
                    projects: ProjectGrant::Selected {
                        space_ids: vec!["sp-granted".into(), "sp-remote".into()],
                    },
                    ..Default::default()
                },
            }),
        )
        .await
        .expect("CreateBot");
    let bot: Bot = serde_json::from_value(value).unwrap();
    let cwd = core
        .workspace
        .chat(&bot.chat_id)
        .unwrap()
        .unwrap()
        .cwd
        .unwrap();
    Rig {
        core,
        client,
        bot,
        cwd,
        granted,
        secret,
        _tmp: tmp,
    }
}

impl Rig {
    async fn call(
        &self,
        method: &str,
        chat_id: &str,
        root: Option<&str>,
        path: &str,
    ) -> Result<serde_json::Value, cypher_rpc::RpcError> {
        let cwd = self
            .core
            .workspace
            .chat(chat_id)
            .unwrap()
            .and_then(|chat| chat.cwd)
            .unwrap_or_default();
        let mut params = serde_json::json!({ "chatId": chat_id, "cwd": cwd, "path": path });
        if let Some(root) = root {
            params["rootSpaceId"] = root.into();
        }
        self.client.call(method, params).await
    }

    async fn roots(&self, chat_id: &str) -> Vec<ChatFileRoot> {
        let value = self
            .client
            .call(
                methods::LIST_CHAT_FILE_ROOTS,
                serde_json::json!({ "chatId": chat_id }),
            )
            .await
            .expect("ListChatFileRoots");
        serde_json::from_value(value).expect("roots")
    }

    /// A project-less chat hosted here in the workspace's folder, as `edit`
    /// marks it (a hidden thread, a memory pass, a forged row).
    fn thread(&self, chat_id: &str, edit: impl FnOnce(&mut Chat)) {
        self.core
            .workspace
            .create_chat(
                chat_id,
                None,
                Some(&self.core.device_id),
                None,
                Some(self.cwd.clone()),
            )
            .unwrap();
        let mut chat = self.core.workspace.chat(chat_id).unwrap().unwrap();
        edit(&mut chat);
        self.core.workspace.import_chat_row(&chat).unwrap();
    }
}

fn canonical(path: impl AsRef<Path>) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn names(listing: &serde_json::Value) -> Vec<String> {
    listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workspace_chat_lists_its_folder_then_its_granted_local_projects() {
    let rig = rig().await;
    let roots = rig.roots(&rig.bot.chat_id).await;
    assert_eq!(
        roots,
        [
            ChatFileRoot {
                space_id: None,
                name: "Billing".into(),
                path: canonical(&rig.cwd),
                checkout_id: None,
            },
            ChatFileRoot {
                space_id: Some("sp-granted".into()),
                name: "granted".into(),
                path: canonical(&rig.granted),
                checkout_id: rig
                    .core
                    .workspace
                    .space("sp-granted")
                    .unwrap()
                    .unwrap()
                    .checkout_id,
            },
        ],
        "the ungranted project and the other device's are left out"
    );

    // A session lists its own checkout and nothing else.
    rig.core
        .workspace
        .create_chat("session", Some("sp-secret"), None, None, None)
        .unwrap();
    let session = rig.roots("session").await;
    assert_eq!(session.len(), 1, "{session:?}");
    assert_eq!(session[0].space_id, None);
    assert_eq!(session[0].name, "secret");
    assert_eq!(session[0].path, canonical(&rig.secret));
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_root_still_serves_sessions_and_now_the_workspace_folder() {
    let rig = rig().await;
    rig.core
        .workspace
        .create_chat("session", Some("sp-secret"), None, None, None)
        .unwrap();
    let listing = rig
        .call(methods::LIST_WORKSPACE_FILES, "session", None, "")
        .await
        .expect("a session lists its checkout");
    assert!(names(&listing).contains(&"plans.txt".to_string()));

    std::fs::write(Path::new(&rig.cwd).join("notes.md"), "# notes\n").unwrap();
    let own = rig
        .call(methods::LIST_WORKSPACE_FILES, &rig.bot.chat_id, None, "")
        .await
        .expect("the workspace chat lists its own folder");
    assert_eq!(names(&own), ["notes.md"]);
    let read = rig
        .call(
            methods::READ_WORKSPACE_FILE,
            &rig.bot.chat_id,
            None,
            "notes.md",
        )
        .await
        .expect("and reads in it");
    assert_eq!(read["text"], "# notes\n");

    // The row's cwd must still be the workspace's own folder.
    let mut chat = rig.core.workspace.chat(&rig.bot.chat_id).unwrap().unwrap();
    chat.cwd = Some(rig.secret.to_string_lossy().into_owned());
    rig.core.workspace.import_chat_row(&chat).unwrap();
    let err = rig
        .call(methods::LIST_WORKSPACE_FILES, &rig.bot.chat_id, None, "")
        .await
        .expect_err("a retargeted workspace chat reads nothing");
    assert!(err.to_string().contains("workspace's folder"), "{err}");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_granted_project_root_lists_reads_writes_searches_and_shows_git() {
    let rig = rig().await;
    let chat = rig.bot.chat_id.clone();
    let root = Some("sp-granted");

    let listing = rig
        .call(methods::LIST_WORKSPACE_FILES, &chat, root, "")
        .await
        .expect("list the granted project");
    let listed = names(&listing);
    assert!(listed.contains(&"a.txt".to_string()), "{listed:?}");
    assert!(!listed.contains(&".git".to_string()), "{listed:?}");
    let read = rig
        .call(methods::READ_WORKSPACE_FILE, &chat, root, "a.txt")
        .await
        .expect("read in it");
    assert_eq!(read["text"], "one\nedited\n");
    let mut write = serde_json::json!({
        "chatId": chat, "cwd": rig.cwd, "path": "a.txt",
        "rootSpaceId": "sp-granted", "text": "one\nsaved\n",
    });
    rig.client
        .call(methods::WRITE_WORKSPACE_FILE, write.take())
        .await
        .expect("save in it");
    assert_eq!(
        std::fs::read_to_string(rig.granted.join("a.txt")).unwrap(),
        "one\nsaved\n"
    );
    let found = rig
        .client
        .call(
            methods::SEARCH_FILES,
            serde_json::json!({ "chatId": chat, "rootSpaceId": "sp-granted", "query": "a.txt" }),
        )
        .await
        .expect("search in it");
    assert_eq!(found[0]["path"], "a.txt");

    // Git: the path-addressed RPCs take the listed root as their cwd, and the
    // working-tree watch follows the project although no session is in it.
    let path = rig.roots(&chat).await[1].path.clone();
    let diff = rig
        .client
        .call(
            methods::GET_CHECKOUT_DIFF,
            serde_json::json!({ "cwd": path }),
        )
        .await
        .expect("GetCheckoutDiff");
    assert!(diff["patch"].as_str().unwrap().contains("+saved"), "{diff}");
    let history = rig
        .client
        .call(
            methods::LIST_GIT_HISTORY,
            serde_json::json!({ "cwd": path }),
        )
        .await
        .expect("ListGitHistory");
    assert_eq!(history["commits"][0]["subject"], "initial");

    rig.core.diff_sync.reconcile_now().await;
    let mut diffs = rig.core.diff_sync.watch_diffs();
    let watched = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(diff) = diffs.borrow_and_update().iter().find(|d| d.cwd == path) {
                break diff.clone();
            }
            diffs.changed().await.expect("watch alive");
        }
    })
    .await
    .expect("the granted project's diff is published");
    assert!(watched.patch.contains("+saved"));
    let secret = canonical(&rig.secret);
    assert!(
        !diffs.borrow().iter().any(|d| d.cwd == secret),
        "an ungranted project without sessions is not watched"
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn other_roots_are_refused() {
    let rig = rig().await;
    let chat = rig.bot.chat_id.clone();
    let refused = |result: Result<serde_json::Value, cypher_rpc::RpcError>, what: &str| {
        let err = result.expect_err(what).to_string();
        assert!(!err.is_empty(), "{what}");
        err
    };

    let err = refused(
        rig.call(
            methods::READ_WORKSPACE_FILE,
            &chat,
            Some("sp-secret"),
            "plans.txt",
        )
        .await,
        "an ungranted project",
    );
    assert!(err.contains("not available"), "{err}");
    let err = refused(
        rig.call(
            methods::LIST_WORKSPACE_FILES,
            &chat,
            Some("no-such-space"),
            "",
        )
        .await,
        "an unknown project",
    );
    assert!(err.contains("not available"), "{err}");
    let err = refused(
        rig.call(methods::LIST_WORKSPACE_FILES, &chat, Some("sp-remote"), "")
            .await,
        "a granted project on another device",
    );
    assert!(err.contains("another device"), "{err}");
    let secret = rig.secret.to_string_lossy().into_owned();
    for root in [secret.as_str(), "/", "/etc", "../secret"] {
        refused(
            rig.call(methods::LIST_WORKSPACE_FILES, &chat, Some(root), "")
                .await,
            "a path is no root",
        );
    }
    refused(
        rig.client
            .call(
                methods::SEARCH_FILES,
                serde_json::json!({ "chatId": chat, "rootSpaceId": "sp-secret", "query": "plans" }),
            )
            .await,
        "search in an ungranted project",
    );
    // An unknown `root` field is no root at all: the request stays in the
    // workspace's own folder.
    let own = rig
        .client
        .call(
            methods::LIST_WORKSPACE_FILES,
            serde_json::json!({ "chatId": chat, "cwd": rig.cwd, "path": "", "root": secret }),
        )
        .await
        .expect("the default root");
    assert!(names(&own).is_empty(), "{own}");

    // A scope edit applies to the next request.
    support::mutate(
        &rig.core,
        serde_json::json!({
            "op": "updateBot", "botId": rig.bot.id, "name": "Billing",
            "scope": BotScope::default(),
        }),
    )
    .await;
    refused(
        rig.call(methods::LIST_WORKSPACE_FILES, &chat, Some("sp-granted"), "")
            .await,
        "a project the scope no longer grants",
    );
    assert_eq!(rig.roots(&chat).await.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_root_keeps_the_file_browser_rules() {
    let rig = rig().await;
    let chat = rig.bot.chat_id.clone();
    let root = Some("sp-granted");
    let outside = rig.secret.join("plans.txt");
    std::os::unix::fs::symlink(&outside, rig.granted.join("leak.txt")).unwrap();
    std::os::unix::fs::symlink(&rig.secret, rig.granted.join("linked")).unwrap();

    for path in [
        "leak.txt",
        "linked/plans.txt",
        "../secret/plans.txt",
        "/etc/hosts",
    ] {
        assert!(
            rig.call(methods::READ_WORKSPACE_FILE, &chat, root, path)
                .await
                .is_err(),
            "{path} escapes the project"
        );
    }
    assert!(
        rig.call(methods::LIST_WORKSPACE_FILES, &chat, root, "linked")
            .await
            .is_err(),
        "a linked folder is not walked"
    );
    assert!(
        rig.call(methods::READ_WORKSPACE_FILE, &chat, root, ".git/config")
            .await
            .is_err(),
        "Git metadata stays unavailable"
    );

    // Every other file reads exactly as it does from a session in the same
    // project: the browser has no rules of its own for the workspace.
    rig.core
        .workspace
        .create_chat("session", Some("sp-granted"), None, None, None)
        .unwrap();
    let from_session = rig
        .call(methods::READ_WORKSPACE_FILE, "session", None, ".env")
        .await
        .map_err(|err| err.to_string());
    let from_workspace = rig
        .call(methods::READ_WORKSPACE_FILE, &chat, root, ".env")
        .await
        .map_err(|err| err.to_string());
    assert_eq!(from_workspace, from_session);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_workspaces_own_chat_may_name_a_project_root() {
    let rig = rig().await;
    let bot = rig.bot.id.clone();
    let marked = |marker: BotChat| move |chat: &mut Chat| chat.bot = Some(marker);
    rig.thread(
        "plan-thread",
        marked(BotChat::thread(
            &bot,
            BotOrigin::Owner,
            BotThread::Work {
                work_id: "w1".into(),
            },
        )),
    );
    rig.thread(
        "routine-thread",
        marked(BotChat::thread(
            &bot,
            BotOrigin::Routine,
            BotThread::Routine {
                routine_id: "r1".into(),
            },
        )),
    );
    rig.thread("peer-thread", marked(BotChat::new(&bot, BotOrigin::Peer)));
    // A row that claims to be the bot's own chat but is not the one it names.
    rig.thread("impostor", marked(BotChat::owner(&bot)));
    rig.thread("memory-pass", |chat| {
        chat.memory = Some(MemoryChat {
            store_id: "store".into(),
            scope: MemoryScope::Workspace {
                bot_id: bot.clone(),
            },
            pass_id: "pass".into(),
        });
    });
    rig.core
        .workspace
        .create_chat("session", Some("sp-granted"), None, None, None)
        .unwrap();

    let hidden = [
        "plan-thread",
        "routine-thread",
        "peer-thread",
        "impostor",
        "memory-pass",
    ];
    for chat in hidden.iter().copied().chain(["session"]) {
        let err = rig
            .call(methods::LIST_WORKSPACE_FILES, chat, Some("sp-granted"), "")
            .await
            .expect_err(chat)
            .to_string();
        assert!(err.contains("workspace's own chat"), "{chat}: {err}");
        assert!(
            rig.client
                .call(
                    methods::SEARCH_FILES,
                    serde_json::json!({ "chatId": chat, "rootSpaceId": "sp-granted", "query": "a" }),
                )
                .await
                .is_err(),
            "{chat} searches no project root"
        );
    }
    for chat in hidden {
        assert!(
            rig.call(methods::LIST_WORKSPACE_FILES, chat, None, "")
                .await
                .is_err(),
            "{chat} still has no file browser"
        );
        assert_eq!(rig.roots(chat).await, [], "{chat} lists no roots");
    }

    // Another workspace's chat cannot reach this one's projects either.
    let other: Bot = serde_json::from_value(
        rig.client
            .call(
                methods::CREATE_BOT,
                serde_json::json!({ "targetDeviceId": rig.core.device_id, "name": "Other" }),
            )
            .await
            .expect("CreateBot"),
    )
    .unwrap();
    assert!(
        rig.call(
            methods::LIST_WORKSPACE_FILES,
            &other.chat_id,
            Some("sp-granted"),
            ""
        )
        .await
        .is_err(),
        "a project only another workspace grants"
    );
    assert_eq!(rig.roots(&other.chat_id).await.len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_on_another_device_gets_no_project_root_here() {
    let rig = rig().await;
    // The workspace row says another device hosts it; this device only
    // holds a copy of its chat row.
    let mut bot = rig.core.workspace.bot(&rig.bot.id).unwrap().unwrap();
    bot.host_device_id = "other-device".into();
    let chat = rig.core.workspace.chat(&rig.bot.chat_id).unwrap().unwrap();
    rig.core.workspace.create_bot(&bot, &chat).unwrap();
    let err = rig
        .call(
            methods::LIST_WORKSPACE_FILES,
            &rig.bot.chat_id,
            Some("sp-granted"),
            "",
        )
        .await
        .expect_err("another device's workspace")
        .to_string();
    assert!(err.contains("workspace's own chat"), "{err}");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_watch_follows_a_scope_edit() {
    let rig = rig().await;
    let secret = canonical(&rig.secret);
    let granted = canonical(&rig.granted);
    let mut diffs = rig.core.diff_sync.watch_diffs();
    // Settled: the first grant is watched and the rows have stopped moving.
    tokio::time::timeout(Duration::from_secs(20), async {
        while !diffs.borrow_and_update().iter().any(|d| d.cwd == granted) {
            diffs.changed().await.expect("watch alive");
        }
    })
    .await
    .expect("the first grant is watched");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!diffs.borrow().iter().any(|d| d.cwd == secret));
    // A scope-only edit (no chat row changes with it), and no reconcile by
    // hand: the workspace edit itself brings the project in.
    rig.core
        .workspace
        .update_bot(
            &rig.bot.id,
            &cypher_doc::BotUpdate {
                scope: Some(BotScope {
                    projects: ProjectGrant::All,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
    let watched = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(diff) = diffs.borrow_and_update().iter().find(|d| d.cwd == secret) {
                break diff.clone();
            }
            diffs.changed().await.expect("watch alive");
        }
    })
    .await
    .expect("a newly granted project is watched");
    assert!(watched.patch.contains("plans.txt"), "{}", watched.patch);
    assert_eq!(rig.roots(&rig.bot.chat_id).await.len(), 3);
    rig.core.shutdown().await;
}
