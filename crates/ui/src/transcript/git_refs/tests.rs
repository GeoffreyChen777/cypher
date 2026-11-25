use std::time::{Duration, Instant};

use cypher_proto::{
    GitCommitPreview, GitRef, GitRefResolution, GitRefUnavailable, GithubChecksState,
    GithubChecksSummary, GithubIssuePreview, GithubPullPreview, GithubRefState,
};

use super::*;
use crate::markdown::git_ref::Unresolved;

fn pull() -> GithubPullPreview {
    GithubPullPreview {
        repo: "acme/app".into(),
        number: 71,
        title: "Relay link fixes".into(),
        state: GithubRefState::Merged,
        author: "octo".into(),
        head_branch: "fix/relay".into(),
        base_branch: "main".into(),
        url: "https://github.com/acme/app/pull/71".into(),
        checks: Some(GithubChecksSummary {
            state: GithubChecksState::Failure,
            passed: 14,
            failed: 2,
            pending: 1,
            skipped: 3,
        }),
        changed_files: Some(3),
        additions: Some(12),
        deletions: Some(4),
    }
}

fn commit() -> GitCommitPreview {
    GitCommitPreview {
        sha: "15d45c7a0000000000000000000000000000beef".into(),
        short_sha: "15d45c7a".into(),
        subject: "iOS: cap tool calls".into(),
        author: "Ada".into(),
        author_time: 1_000_000,
        files_changed: 1,
        insertions: 5,
        deletions: 0,
        url: None,
    }
}

fn key(chat: &str, number: u64) -> CardKey {
    CardKey {
        context: RefContext {
            chat_id: chat.into(),
            target: None,
            root_space_id: None,
        },
        git_ref: GitRef::Issue { repo: None, number },
    }
}

#[test]
fn a_pull_request_card_says_each_thing_once() {
    let model = card_model(&GitRefResolution::PullRequest(pull()), 0).unwrap();
    assert_eq!(model.id, "acme/app #71");
    assert_eq!(model.mark, Some("Merged"));
    assert_eq!(model.title, "Relay link fixes");
    assert_eq!(
        model.details,
        vec![
            "octo · fix/relay \u{2192} main".to_string(),
            "Checks: 2 failed, 1 running, 14 passed".to_string(),
            "3 files · +12 \u{2212}4".to_string(),
        ]
    );
    let mut bare = pull();
    bare.checks = None;
    bare.additions = None;
    let model = card_model(&GitRefResolution::PullRequest(bare), 0).unwrap();
    assert_eq!(model.details.len(), 1);
}

#[test]
fn issue_and_commit_cards() {
    let issue = GithubIssuePreview {
        repo: "acme/app".into(),
        number: 9,
        title: "Crash on launch".into(),
        state: GithubRefState::Closed,
        state_reason: Some("not_planned".into()),
        author: "bob".into(),
        url: "https://github.com/acme/app/issues/9".into(),
    };
    let model = card_model(&GitRefResolution::Issue(issue.clone()), 0).unwrap();
    assert_eq!(model.mark, Some("Not planned"));
    assert_eq!(model.details, vec!["bob".to_string()]);
    let completed = GithubIssuePreview {
        state_reason: Some("completed".into()),
        ..issue
    };
    let model = card_model(&GitRefResolution::Issue(completed), 0).unwrap();
    assert_eq!(model.mark, Some("Closed"));

    let model = card_model(&GitRefResolution::Commit(commit()), 1_000_000 + 7_200).unwrap();
    assert_eq!(model.id, "15d45c7a");
    assert_eq!(model.mark, None);
    assert_eq!(
        model.details,
        vec![
            "Ada · 2 hours ago".to_string(),
            "1 file · +5 \u{2212}0".to_string()
        ]
    );
}

#[test]
fn nothing_found_or_unreachable_shows_no_card() {
    let not_found = GitRefResolution::NotFound { url: None };
    let unavailable = GitRefResolution::Unavailable {
        reason: GitRefUnavailable::SignedOut,
        url: Some("https://github.com/acme/app/pull/3".into()),
    };
    assert!(card_model(&not_found, 0).is_none());
    assert!(card_model(&unavailable, 0).is_none());
    // Only a reference that names nothing goes back to plain text.
    assert!(names_nothing(&not_found));
    assert!(!names_nothing(&unavailable));
    assert!(!names_nothing(&GitRefResolution::Commit(commit())));
}

#[test]
fn checks_say_what_needs_attention_first() {
    let summary = |passed, failed, pending, skipped| GithubChecksSummary {
        state: GithubChecksState::Success,
        passed,
        failed,
        pending,
        skipped,
    };
    assert_eq!(
        checks_text(&summary(3, 0, 0, 1)).as_deref(),
        Some("Checks: 3 passed")
    );
    assert_eq!(
        checks_text(&summary(0, 0, 0, 2)).as_deref(),
        Some("Checks: 2 skipped")
    );
    assert_eq!(checks_text(&summary(0, 0, 0, 0)), None);
}

#[test]
fn times_read_coarsely() {
    let now = 10_000_000;
    assert_eq!(relative_time(now - 5, now), "just now");
    assert_eq!(relative_time(now + 50, now), "just now");
    assert_eq!(relative_time(now - 60, now), "1 minute ago");
    assert_eq!(relative_time(now - 3 * 3600, now), "3 hours ago");
    assert_eq!(relative_time(now - 86_400, now), "yesterday");
    assert_eq!(relative_time(now - 5 * 86_400, now), "5 days ago");
    assert_eq!(relative_time(now - 400 * 86_400, now), "1 year ago");
}

#[test]
fn lookups_go_stale() {
    let mut cache = GitRefCache::default();
    let at = Instant::now();
    cache.begin(key("c", 1), at);
    assert_eq!(cache.get(&key("c", 1), at), Some(&Lookup::Pending));
    // A lookup that never answered is asked again.
    assert_eq!(cache.get(&key("c", 1), at + PENDING_FOR), None);
    let found = GitRefResolution::NotFound { url: None };
    cache.settle(key("c", 1), found.clone(), at);
    assert_eq!(
        cache.get(&key("c", 1), at + PENDING_FOR),
        Some(&Lookup::Done(found))
    );
    assert_eq!(cache.get(&key("c", 1), at + FRESH_FOR), None);
    // Per chat.
    assert_eq!(cache.get(&key("d", 1), at), None);
}

#[test]
fn the_cache_keeps_its_budget() {
    let mut cache = GitRefCache::default();
    let at = Instant::now();
    for n in 0..(CACHE_BUDGET as u64 + 5) {
        cache.begin(key("c", n), at + Duration::from_millis(n));
    }
    assert_eq!(cache.entries.len(), CACHE_BUDGET);
    // The oldest went first.
    assert!(cache.get(&key("c", 0), at).is_none());
    assert!(cache.get(&key("c", CACHE_BUDGET as u64 + 4), at).is_some());
}

#[test]
fn unresolved_refs_hide_per_chat() {
    let mut unresolved = Unresolved::default();
    let issue = GitRef::Issue {
        repo: None,
        number: 4,
    };
    unresolved.set_chat("a");
    let before = unresolved.revision();
    assert!(unresolved.insert("a", issue.clone()));
    assert!(!unresolved.insert("a", issue.clone()));
    assert!(unresolved.revision() > before);
    assert!(unresolved.hides(&issue));
    unresolved.set_chat("b");
    assert!(!unresolved.hides(&issue));
    unresolved.set_chat("a");
    assert!(unresolved.hides(&issue));
}

#[test]
fn a_looked_up_commit_opens_by_its_sha() {
    let history = history_commit(&commit());
    assert_eq!(history.sha, commit().sha);
    assert_eq!(history.subject, "iOS: cap tool calls");
    assert!(history.authored_at.starts_with("1970-01-12"));
}

#[test]
fn a_card_opens_where_it_fits_without_covering_its_link() {
    // Room for the tallest card below: below, however much is above.
    assert!(card_goes_below(CARD_ROOM, 600.0));
    // Near the bottom of the window (a pull request card would be snapped
    // up over the link): above, where there is more room.
    assert!(!card_goes_below(165.0, 670.0));
    // Near the top: below, even when short of the tallest card's room.
    assert!(card_goes_below(150.0, 60.0));
}
