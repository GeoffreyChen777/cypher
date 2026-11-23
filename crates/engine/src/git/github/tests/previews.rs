//! `ResolveGitRef`'s GitHub half: pull request / issue cards, checks, the
//! cache and the quiet failures — against the scripted GitHub, no network.

use super::*;

use cypher_proto::{
    GitRefResolution, GitRefUnavailable, GithubChecksState, GithubChecksSummary,
    GithubIssuePreview, GithubPullPreview, GithubRefState,
};

use super::super::previews::{
    RestCheckRun, RestPull, RestStatus, TtlCache, checks_summary, issue_preview, pull_preview,
};

fn pull_json(number: u64) -> serde_json::Value {
    json!({
        "number": number,
        "title": format!("PR {number}"),
        "state": "open",
        "draft": false,
        "merged": false,
        "merged_at": null,
        "user": {"login": "octo"},
        "head": {"ref": "topic", "label": "o:topic", "sha": "abc123def", "repo": {"full_name": "o/r"}},
        "base": {"ref": "main", "label": "o:main", "sha": "0011aa", "repo": {"full_name": "o/r"}},
        "html_url": format!("https://github.com/o/r/pull/{number}"),
        "changed_files": 3,
        "additions": 10,
        "deletions": 2,
    })
}

fn signed_in(mock: &MockGithub) -> (tempfile::TempDir, Github) {
    let data = tempfile::tempdir().unwrap();
    write_account(data.path(), &account("ghu_1", None, None));
    let github = Github::new(mock.config(Some("Iv1.test")), data.path());
    (data, github)
}

#[tokio::test]
async fn a_pull_request_card_carries_branches_and_checks() {
    let mock = MockGithub::start(|req| match req.path.as_str() {
        "/repos/o/r" => (200, json!({})),
        "/repos/o/r/pulls/5" => (200, pull_json(5)),
        "/repos/o/r/commits/abc123def/check-runs" => (
            200,
            json!({"total_count": 4, "check_runs": [
                {"status": "completed", "conclusion": "success"},
                {"status": "completed", "conclusion": "failure"},
                {"status": "in_progress", "conclusion": null},
                {"status": "completed", "conclusion": "skipped"},
            ]}),
        ),
        "/repos/o/r/commits/abc123def/status" => (
            200,
            json!({"state": "success", "statuses": [{"state": "success"}]}),
        ),
        _ => (404, json!({})),
    })
    .await;
    let (_data, github) = signed_in(&mock);
    let checkout = git_checkout("https://github.com/o/r.git");
    let resolution = github.number_preview("o/r", 5, Some(checkout.path())).await;
    assert_eq!(
        resolution,
        GitRefResolution::PullRequest(GithubPullPreview {
            repo: "o/r".into(),
            number: 5,
            title: "PR 5".into(),
            state: GithubRefState::Open,
            author: "octo".into(),
            head_branch: "topic".into(),
            base_branch: "main".into(),
            url: "https://github.com/o/r/pull/5".into(),
            checks: Some(GithubChecksSummary {
                state: GithubChecksState::Failure,
                passed: 2,
                failed: 1,
                pending: 1,
                skipped: 1,
            }),
            changed_files: Some(3),
            additions: Some(10),
            deletions: Some(2),
        })
    );
    assert_eq!(
        mock.requests("/repos/o/r/commits/abc123def/check-runs")[0].query["per_page"],
        "100"
    );
    assert_eq!(
        mock.requests("/repos/o/r/pulls/5")[0].auth.as_deref(),
        Some("Bearer ghu_1")
    );
    assert!(
        mock.requests("/repos/o/r/issues/5").is_empty(),
        "a pull request needs no issue request"
    );
}

#[tokio::test]
async fn an_issue_number_becomes_an_issue_card_and_unknown_numbers_not_found() {
    let mock = MockGithub::start(|req| match req.path.as_str() {
        "/repos/o/r" => (200, json!({})),
        "/repos/o/r/issues/6" => {
            let mut issue = issue_json(6, "o/r", false);
            issue["state"] = json!("closed");
            issue["state_reason"] = json!("not_planned");
            (200, issue)
        }
        _ => (404, json!({})),
    })
    .await;
    let (_data, github) = signed_in(&mock);
    assert_eq!(
        github.number_preview("o/r", 6, None).await,
        GitRefResolution::Issue(GithubIssuePreview {
            repo: "o/r".into(),
            number: 6,
            title: "Issue 6".into(),
            state: GithubRefState::Closed,
            state_reason: Some("not_planned".into()),
            author: "someone".into(),
            url: "https://github.com/o/r/issues/6".into(),
        })
    );
    let not_found = GitRefResolution::NotFound {
        url: Some(format!("{}/o/r/issues/404", mock.base)),
    };
    assert_eq!(github.number_preview("o/r", 404, None).await, not_found);
    // Both answers are cached: asking again sends nothing.
    let sent = mock.requests.lock().unwrap().len();
    assert!(
        matches!(
            github.number_preview("O/R", 6, None).await,
            GitRefResolution::Issue(GithubIssuePreview { number: 6, .. })
        ),
        "keyed case-insensitively"
    );
    assert_eq!(github.number_preview("o/r", 404, None).await, not_found);
    assert_eq!(mock.requests.lock().unwrap().len(), sent);
}

#[tokio::test]
async fn failures_are_quiet_keep_the_link_and_are_not_cached() {
    let mock = MockGithub::start(|req| match req.path.as_str() {
        "/repos/o/r" => (200, json!({})),
        "/repos/o/r/pulls/1" => (500, json!({})),
        "/repos/hidden/r" => (404, json!({})),
        _ => (404, json!({})),
    })
    .await;
    let (_data, github) = signed_in(&mock);
    let link = |repo: &str, number: u64| Some(format!("{}/{repo}/issues/{number}", mock.base));
    for _ in 0..2 {
        assert_eq!(
            github.number_preview("o/r", 1, None).await,
            GitRefResolution::Unavailable {
                reason: GitRefUnavailable::Unreachable,
                url: link("o/r", 1),
            }
        );
    }
    assert_eq!(
        mock.requests("/repos/o/r/pulls/1").len(),
        2,
        "a failure is retried on the next hover"
    );
    assert_eq!(
        github.number_preview("hidden/r", 2, None).await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::NoAccess,
            url: link("hidden/r", 2),
        }
    );

    // No credential at all.
    let data = tempfile::tempdir().unwrap();
    let signed_out = Github::new(mock.config(Some("Iv1.test")), data.path());
    assert_eq!(
        signed_out.number_preview("o/r", 3, None).await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::SignedOut,
            url: link("o/r", 3),
        }
    );
}

#[tokio::test]
async fn a_github_that_never_answers_times_out_quietly() {
    // Accepts connections and holds them without a response.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let held = tokio::spawn(async move {
        let mut sockets = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            sockets.push(socket);
        }
    });
    let data = tempfile::tempdir().unwrap();
    write_account(data.path(), &account("ghu_1", None, None));
    let config = GithubConfig {
        client_id: Some("Iv1.test".into()),
        app_slug: None,
        api_base: base.clone(),
        web_base: base.clone(),
        local_fallbacks: false,
        poll_unit: Duration::from_millis(5),
        preview_timeout: Duration::from_millis(200),
    };
    let github = Github::new(config, data.path());
    let started = Instant::now();
    assert_eq!(
        github.number_preview("o/r", 9, None).await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::Unreachable,
            url: Some(format!("{base}/o/r/issues/9")),
        }
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    held.abort();
}

#[test]
fn the_cache_expires_entries_and_stays_bounded() {
    let start = Instant::now();
    let ttl = Duration::from_secs(180);
    let mut cache: TtlCache<u64, &str> = TtlCache::new(ttl, 2);
    cache.insert(1, "one", start);
    assert_eq!(cache.get(&1, start + Duration::from_secs(179)), Some("one"));
    assert_eq!(cache.get(&1, start + ttl), None, "expired at the TTL");
    assert_eq!(cache.len(), 0, "an expired entry is dropped when read");

    cache.insert(1, "one", start);
    cache.insert(2, "two", start + Duration::from_secs(1));
    cache.insert(3, "three", start + Duration::from_secs(2));
    assert_eq!(cache.len(), 2);
    let now = start + Duration::from_secs(3);
    assert_eq!(cache.get(&1, now), None, "the oldest made room");
    assert_eq!(cache.get(&2, now), Some("two"));
    assert_eq!(cache.get(&3, now), Some("three"));

    // A full cache drops expired entries before live ones.
    let later = start + Duration::from_secs(1) + ttl;
    cache.insert(4, "four", later);
    assert_eq!(cache.get(&3, later), Some("three"));
    assert_eq!(cache.get(&4, later), Some("four"));
    // Replacing a key never evicts another.
    cache.insert(4, "FOUR", later);
    assert_eq!(cache.get(&3, later), Some("three"));
    assert_eq!(cache.get(&4, later), Some("FOUR"));
}

#[test]
fn pull_json_maps_to_card_states() {
    let preview = |patch: serde_json::Value| {
        let mut value = pull_json(7);
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        let pull: RestPull = serde_json::from_value(value).unwrap();
        pull_preview("o/r", pull, None)
    };
    assert_eq!(preview(json!({})).state, GithubRefState::Open);
    assert_eq!(preview(json!({"draft": true})).state, GithubRefState::Draft);
    assert_eq!(
        preview(json!({"state": "closed", "draft": true})).state,
        GithubRefState::Closed
    );
    assert_eq!(
        preview(json!({"state": "closed", "merged": true})).state,
        GithubRefState::Merged
    );
    assert_eq!(
        preview(json!({"state": "closed", "merged_at": "2026-11-01T00:00:00Z"})).state,
        GithubRefState::Merged
    );
    // A fork's branch carries its owner; a deleted fork keeps the label.
    let fork = preview(
        json!({"head": {"ref": "fix", "label": "fan:fix", "sha": "aa", "repo": {"full_name": "fan/r"}}}),
    );
    assert_eq!(fork.head_branch, "fan:fix");
    let gone =
        preview(json!({"head": {"ref": "fix", "label": "fan:fix", "sha": "aa", "repo": null}}));
    assert_eq!(gone.head_branch, "fan:fix");
    assert_eq!(preview(json!({"user": null})).author, "ghost");
    // Optional stats may be absent.
    let mut bare = pull_json(8);
    for key in ["changed_files", "additions", "deletions", "draft", "merged"] {
        bare.as_object_mut().unwrap().remove(key);
    }
    let bare = pull_preview("o/r", serde_json::from_value(bare).unwrap(), None);
    assert_eq!(bare.changed_files, None);
    assert_eq!(bare.state, GithubRefState::Open);
}

#[test]
fn issue_json_maps_to_an_issue_card() {
    let mut value = issue_json(3, "o/r", false);
    value["user"] = serde_json::Value::Null;
    let issue: RestIssue = serde_json::from_value(value).unwrap();
    let card = issue_preview("o/r", issue);
    assert_eq!(card.state, GithubRefState::Open);
    assert_eq!(card.state_reason, None);
    assert_eq!(card.author, "ghost");
}

#[test]
fn checks_roll_up_like_github() {
    let run = |status: &str, conclusion: Option<&str>| RestCheckRun {
        status: status.into(),
        conclusion: conclusion.map(str::to_string),
    };
    let status = |state: &str| RestStatus {
        state: state.into(),
    };
    assert_eq!(checks_summary(&[], &[]), None, "no checks, no summary");
    let passing = checks_summary(
        &[
            run("completed", Some("success")),
            run("completed", Some("neutral")),
        ],
        &[status("success")],
    )
    .unwrap();
    assert_eq!(passing.state, GithubChecksState::Success);
    assert_eq!((passing.passed, passing.skipped), (2, 1));
    let pending = checks_summary(
        &[run("queued", None), run("completed", Some("stale"))],
        &[status("pending"), status("success")],
    )
    .unwrap();
    assert_eq!(pending.state, GithubChecksState::Pending);
    assert_eq!((pending.pending, pending.passed), (3, 1));
    for conclusion in ["failure", "timed_out", "cancelled", "action_required"] {
        let failing = checks_summary(
            &[run("completed", Some(conclusion)), run("queued", None)],
            &[],
        )
        .unwrap();
        assert_eq!(failing.state, GithubChecksState::Failure, "{conclusion}");
    }
    let errored = checks_summary(&[], &[status("error")]).unwrap();
    assert_eq!(errored.failed, 1);
}
