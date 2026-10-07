use super::*;

fn chat(id: &str, branch: Option<&str>, cwd: Option<&str>) -> (ChatIndicator, Chat) {
    (
        ChatIndicator::Idle,
        Chat {
            id: id.into(),
            cwd: cwd.map(Into::into),
            branch: branch.map(Into::into),
            created_at: Utc::now(),
            ..crate::test_fixtures::chat()
        },
    )
}

/// `group_chats` cases: each row's chats as `(id, branch, cwd)` under a
/// `/repo` space, and the expected groups in order as
/// `(label, branch, worktree, worktree_path, chat count)`.
#[test]
fn group_chats_by_checkout_identity() {
    type ChatRow<'a> = (&'a str, Option<&'a str>, Option<&'a str>);
    type GroupShape<'a> = (&'a str, Option<&'a str>, bool, Option<&'a str>, usize);
    let cases: &[(&str, &[ChatRow], &[GroupShape])] = &[
        (
            // Two worktrees whose chats carry the same branch label but
            // different paths are DIFFERENT checkouts — they must not
            // merge (each header gets its own add button and collapse);
            // branch metadata is preserved for targeting new sessions.
            "same-label detached worktrees stay distinct",
            &[
                ("a", Some("detached"), Some("/repo/.worktrees/one")),
                ("b", Some("detached"), Some("/repo/.worktrees/two")),
                ("c", Some("detached"), Some("/repo/.worktrees/one")),
            ],
            &[
                (
                    "detached",
                    Some("detached"),
                    true,
                    Some("/repo/.worktrees/one"),
                    2,
                ),
                (
                    "detached",
                    Some("detached"),
                    true,
                    Some("/repo/.worktrees/two"),
                    1,
                ),
            ],
        ),
        (
            // A `main` chat in the space root and a `main` worktree chat
            // are different checkouts — distinct groups and collapse keys.
            "ordinary vs worktree with the same label never merge",
            &[
                ("a", Some("main"), Some("/repo")),
                ("b", Some("main"), Some("/repo/.worktrees/main")),
            ],
            &[
                ("main", Some("main"), false, None, 1),
                ("main", Some("main"), true, Some("/repo/.worktrees/main"), 1),
            ],
        ),
        (
            // Worktree detection trims trailing slashes: "/repo/" is the
            // space root, not a worktree — and the worktree path stays
            // EXACT (not trimmed) as the add-button target.
            "trailing slashes are normalized for worktree detection",
            &[
                ("a", Some("main"), Some("/repo/")),
                ("b", Some("feat"), Some("/repo/.worktrees/feat/")),
            ],
            &[
                ("main", Some("main"), false, None, 1),
                (
                    "feat",
                    Some("feat"),
                    true,
                    Some("/repo/.worktrees/feat/"),
                    1,
                ),
            ],
        ),
        (
            // A branch-less/blank chat is the ordinary current checkout:
            // the label falls back, but the carried branch stays None (a
            // new session targets the checkout with no branch to name).
            "blank branch falls back to the current checkout label",
            &[("a", None, Some("/repo")), ("b", Some("  "), Some("/repo"))],
            &[("Current checkout", None, false, None, 2)],
        ),
        (
            // `/wt` and `/wt/` are the SAME checkout — one group, while
            // the first appearance's EXACT raw cwd is the target.
            "trailing-slash worktree folds into one group",
            &[
                ("a", Some("detached"), Some("/repo/.worktrees/wt")),
                ("b", Some("detached"), Some("/repo/.worktrees/wt/")),
            ],
            &[(
                "detached",
                Some("detached"),
                true,
                Some("/repo/.worktrees/wt"),
                2,
            )],
        ),
        (
            // A trailing SPACE is valid path content, not a separator:
            // `/wt` and `/wt ` are distinct identities.
            "trailing space stays part of the worktree identity",
            &[
                ("a", Some("detached"), Some("/repo/.worktrees/wt")),
                ("b", Some("detached"), Some("/repo/.worktrees/wt ")),
            ],
            &[
                (
                    "detached",
                    Some("detached"),
                    true,
                    Some("/repo/.worktrees/wt"),
                    1,
                ),
                (
                    "detached",
                    Some("detached"),
                    true,
                    Some("/repo/.worktrees/wt "),
                    1,
                ),
            ],
        ),
    ];
    for (name, chats, expected) in cases {
        let groups = group_chats(
            chats
                .iter()
                .map(|(id, branch, cwd)| chat(id, *branch, *cwd))
                .collect(),
            Some("/repo"),
        );
        let shapes: Vec<_> = groups
            .iter()
            .map(|g| {
                (
                    g.label.as_str(),
                    g.branch.as_deref(),
                    g.worktree,
                    g.worktree_path.as_deref(),
                    g.chats.len(),
                )
            })
            .collect();
        assert_eq!(shapes, *expected, "{name}");
    }
}

#[test]
fn branch_group_key_includes_worktree_path() {
    // The collapse key must distinguish same-label detached worktrees,
    // mirroring the grouping identity.
    let a = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/one"));
    let b = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/two"));
    let root = Shell::branch_group_key("s:1", false, "main", None);
    assert_ne!(a, b);
    assert_ne!(a, root);
    // Same identity → same key (stable across renders).
    assert_eq!(
        a,
        Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/one"))
    );
}

#[test]
fn branch_group_key_normalizes_trailing_slashes_for_a_stable_collapse_key() {
    // `/wt` and `/wt/` are the same checkout: same disclosure key, so
    // collapsing one collapses the other (status churn never re-opens it).
    let a = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/wt"));
    let with_slash = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/wt/"));
    assert_eq!(a, with_slash);
    // A trailing SPACE is part of the identity — stays distinct from the
    // bare path (whitespace is never trimmed).
    let spaced = Shell::branch_group_key("s:1", true, "detached", Some("/repo/.worktrees/wt "));
    assert_ne!(a, spaced);
}

fn space(id: &str, path: &str) -> Space {
    Space {
        id: id.into(),
        path: path.into(),
        git_detected: true,
        created_at: Utc::now(),
        ..crate::test_fixtures::space()
    }
}

fn session(id: &str, space_id: &str, cwd: &str, branch: Option<&str>) -> Chat {
    let mut row = chat(id, branch, Some(cwd)).1;
    row.space_id = Some(space_id.into());
    row
}

fn child_of(mut row: Chat, parent_id: &str) -> Chat {
    row.child = Some(cypher_proto::ChildChat {
        parent_chat_id: parent_id.into(),
        parent_run_id: "run".into(),
        agent: "helper".into(),
        task: "task".into(),
        mode: cypher_proto::SubagentRunMode::Async,
        tool_call_id: None,
        profile: cypher_proto::ChildAgentProfile {
            system_prompt: String::new(),
            tools: vec![],
            model: None,
            thinking: None,
        },
    });
    row
}

/// `orphan_worktree_after_delete` cases: the chats in a `/repo` space,
/// the deleted id, and the orphaned `(worktree_path, label)` if any.
#[test]
fn orphan_worktree_after_delete_cases() {
    let spaces = [space("s1", "/repo")];
    let feat = |id: &str| session(id, "s1", "/repo/.worktrees/feat", Some("feat"));
    let archived = Chat {
        archived: true,
        ..feat("b")
    };
    let other_device = Chat {
        device_id: "other-dev".into(),
        ..feat("b")
    };
    type Case<'a> = (&'a str, Vec<Chat>, &'a str, Option<(&'a str, &'a str)>);
    let orphan = Some(("/repo/.worktrees/feat", "feat"));
    let cases: Vec<Case> = vec![
        (
            "last session on a linked checkout",
            vec![feat("a")],
            "a",
            orphan,
        ),
        (
            "root checkout sessions are skipped",
            vec![session("a", "s1", "/repo", Some("main"))],
            "a",
            None,
        ),
        (
            "a sibling session shares the (normalized) path",
            vec![
                feat("a"),
                session("b", "s1", "/repo/.worktrees/feat/", Some("feat")),
            ],
            "a",
            None,
        ),
        (
            "archived siblings count",
            vec![feat("a"), archived],
            "a",
            None,
        ),
        (
            "cascaded children of the deleted chat are ignored",
            vec![feat("a"), child_of(feat("child"), "a")],
            "a",
            orphan,
        ),
        (
            "someone else's child on the same path counts",
            vec![feat("a"), child_of(feat("child"), "other-parent")],
            "a",
            None,
        ),
        (
            "distinct worktrees do not block each other",
            vec![
                session("a", "s1", "/repo/.worktrees/one", Some("one")),
                session("b", "s1", "/repo/.worktrees/two", Some("two")),
            ],
            "a",
            Some(("/repo/.worktrees/one", "one")),
        ),
        ("an unknown chat", vec![feat("a")], "missing", None),
        (
            "the same path on another device does not block",
            vec![feat("a"), other_device],
            "a",
            orphan,
        ),
        (
            "a projectless chat",
            vec![chat("a", Some("feat"), Some("/repo/.worktrees/feat")).1],
            "a",
            None,
        ),
    ];
    for (name, chats, deleted, expected) in cases {
        let found = orphan_worktree_after_delete(&chats, &spaces, deleted);
        assert_eq!(
            found
                .as_ref()
                .map(|o| (o.worktree_path.as_str(), o.label.as_str())),
            expected,
            "{name}"
        );
        if let Some(o) = found {
            assert_eq!(o.repo_path, "/repo", "{name}");
            assert_eq!(o.device_id, "dev", "{name}");
        }
    }
}
