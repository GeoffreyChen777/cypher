//! `ResolveGitRef` over the memory transport: commits from the chat's
//! checkout, issue numbers against its GitHub remote, and the quiet
//! answers when a card can't be made. No network: the engine's GitHub has no
//! credential, so number lookups stop before any request.

use crate::support;

use std::path::PathBuf;
use std::time::Duration;

use cypher_engine::git::github::{Github, GithubConfig};
use cypher_rpc::methods;
use serde_json::json;

use support::git;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_git_ref_answers_for_the_chats_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let mut core = support::assemble_mock(&data);
    // Signed out, no `gh`/git credential fallback: a number lookup ends at
    // "no credential" without touching the network.
    core.github = Github::new(
        GithubConfig {
            client_id: None,
            app_slug: None,
            api_base: "http://127.0.0.1:9".into(),
            web_base: "https://github.com".into(),
            local_fallbacks: false,
            poll_unit: Duration::from_millis(5),
            preview_timeout: Duration::from_secs(2),
        },
        &data,
    );
    let client = cypher_rpc::memory_client(core.rpc_service());
    let created = client
        .call(methods::CREATE_REPO, json!({ "name": "demo" }))
        .await
        .expect("CreateRepo");
    let repo_path = created["path"].as_str().expect("repo path").to_string();
    let repo_dir = PathBuf::from(&repo_path);
    std::fs::write(repo_dir.join("file.txt"), "hello\nworld\n").expect("seed file");
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "Seed the demo"]).await;
    let head = git(&repo_dir, &["rev-parse", "HEAD"]).await;
    let head = head.trim();
    core.workspace
        .create_space("space-refs", &core.device_id, &repo_path, None, true)
        .expect("space");
    core.workspace
        .create_chat("refs-chat", Some("space-refs"), None, None, None)
        .expect("chat");
    let resolve = |git_ref: serde_json::Value| {
        client.call(
            methods::RESOLVE_GIT_REF,
            json!({ "chatId": "refs-chat", "ref": git_ref }),
        )
    };

    let commit = resolve(json!({ "kind": "commit", "sha": &head[..8] }))
        .await
        .expect("ResolveGitRef commit");
    assert_eq!(commit["kind"], "commit");
    assert_eq!(commit["sha"], head);
    assert_eq!(commit["subject"], "Seed the demo");
    assert_eq!(commit["author"], "test");
    assert_eq!(commit["filesChanged"], 1);
    assert_eq!(commit["insertions"], 2);
    assert!(commit["authorTime"].as_i64().unwrap() > 0);
    assert!(commit.get("url").is_none(), "no GitHub remote, no page");

    assert_eq!(
        resolve(json!({ "kind": "commit", "sha": "abcdef1234" }))
            .await
            .expect("missing commit"),
        json!({ "kind": "notFound" })
    );
    assert_eq!(
        resolve(json!({ "kind": "issue", "number": 5 }))
            .await
            .expect("number without a remote"),
        json!({ "kind": "unavailable", "reason": "noGithubRemote" })
    );

    git(
        &repo_dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    )
    .await;
    assert_eq!(
        resolve(json!({ "kind": "issue", "number": 5 }))
            .await
            .expect("number with a remote"),
        json!({
            "kind": "unavailable",
            "reason": "signedOut",
            "url": "https://github.com/acme/demo/issues/5"
        })
    );
    assert_eq!(
        resolve(json!({ "kind": "issue", "repo": "rust-lang/rust", "number": 1 }))
            .await
            .expect("qualified number")["url"],
        "https://github.com/rust-lang/rust/issues/1"
    );
    let linked = resolve(json!({ "kind": "commit", "sha": head }))
        .await
        .expect("commit with a remote");
    assert_eq!(
        linked["url"],
        format!("https://github.com/acme/demo/commit/{head}")
    );

    // Malformed refs and unknown chats are errors, not cards.
    for bad in [
        json!({ "kind": "commit", "sha": "--output=/tmp/x" }),
        json!({ "kind": "commit", "sha": "ABCDEF1" }),
        json!({ "kind": "issue", "number": 0 }),
        json!({ "kind": "issue", "repo": "../etc", "number": 1 }),
        json!({ "kind": "branch", "name": "main" }),
    ] {
        assert!(resolve(bad.clone()).await.is_err(), "{bad}");
    }
    assert!(
        client
            .call(
                methods::RESOLVE_GIT_REF,
                json!({ "chatId": "no-such-chat", "ref": { "kind": "issue", "number": 1 } }),
            )
            .await
            .is_err()
    );

    core.shutdown().await;
}
