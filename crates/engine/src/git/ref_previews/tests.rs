use super::*;

use std::path::PathBuf;

fn git_sync(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Tess Author")
        .env("GIT_AUTHOR_EMAIL", "tess@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Tess Author")
        .env("GIT_COMMITTER_EMAIL", "tess@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct Checkout {
    _dir: tempfile::TempDir,
    path: PathBuf,
    root: String,
    second: String,
    merge: String,
    empty: String,
}

/// root: a.txt (2 lines) → second: a.txt edited, b.txt added → a side branch
/// merged with c.txt → an empty commit.
fn checkout(remote: Option<&str>) -> Checkout {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    git_sync(&path, &["init", "-q", "-b", "main"]);
    if let Some(remote) = remote {
        git_sync(&path, &["remote", "add", "origin", remote]);
    }
    std::fs::write(path.join("a.txt"), "one\ntwo\n").unwrap();
    git_sync(&path, &["add", "."]);
    git_sync(
        &path,
        &["commit", "-q", "-m", "Root commit\n\nWith a body."],
    );
    let root = git_sync(&path, &["rev-parse", "HEAD"]);
    std::fs::write(path.join("a.txt"), "one\n2\nthree\n").unwrap();
    std::fs::write(path.join("b.txt"), "b\n").unwrap();
    git_sync(&path, &["add", "."]);
    git_sync(&path, &["commit", "-q", "-m", "Second: edit a, add b"]);
    let second = git_sync(&path, &["rev-parse", "HEAD"]);
    git_sync(&path, &["checkout", "-q", "-b", "side"]);
    std::fs::write(path.join("c.txt"), "c1\nc2\n").unwrap();
    git_sync(&path, &["add", "."]);
    git_sync(&path, &["commit", "-q", "-m", "Side"]);
    git_sync(&path, &["checkout", "-q", "main"]);
    git_sync(
        &path,
        &["commit", "-q", "--allow-empty", "-m", "Main moves"],
    );
    git_sync(
        &path,
        &["merge", "-q", "--no-ff", "-m", "Merge side", "side"],
    );
    let merge = git_sync(&path, &["rev-parse", "HEAD"]);
    git_sync(&path, &["commit", "-q", "--allow-empty", "-m", "Empty"]);
    let empty = git_sync(&path, &["rev-parse", "HEAD"]);
    Checkout {
        _dir: dir,
        path,
        root,
        second,
        merge,
        empty,
    }
}

fn commit(sha: &str) -> GitRef {
    GitRef::Commit {
        repo: None,
        sha: sha.into(),
    }
}

fn found(lookup: CommitLookup) -> GitCommitPreview {
    match lookup {
        CommitLookup::Found(commit) => commit,
        other => panic!("expected a commit, got {other:?}"),
    }
}

#[tokio::test]
async fn commits_read_like_git_show_against_the_first_parent() {
    let repo = checkout(None);
    let second = found(commit_preview(&repo.path, &repo.second).await);
    assert_eq!(second.sha, repo.second);
    assert!(repo.second.starts_with(&second.short_sha));
    assert!(second.short_sha.len() >= 7);
    assert_eq!(second.subject, "Second: edit a, add b");
    assert_eq!(second.author, "Tess Author");
    assert_eq!(second.author_time, 1_700_000_000);
    assert_eq!(
        (second.files_changed, second.insertions, second.deletions),
        (2, 3, 1)
    );
    assert_eq!(second.url, None, "the remote-less checkout has no page");

    // An abbreviation finds the same commit.
    let short = found(commit_preview(&repo.path, &repo.second[..9]).await);
    assert_eq!(short.sha, repo.second);

    let root = found(commit_preview(&repo.path, &repo.root).await);
    assert_eq!(root.subject, "Root commit", "only the first line");
    assert_eq!(
        (root.files_changed, root.insertions, root.deletions),
        (1, 2, 0),
        "a root commit diffs against the empty tree"
    );
    let merge = found(commit_preview(&repo.path, &repo.merge).await);
    assert_eq!(
        (merge.files_changed, merge.insertions, merge.deletions),
        (1, 2, 0),
        "a merge diffs against its first parent"
    );
    let empty = found(commit_preview(&repo.path, &repo.empty).await);
    assert_eq!(
        (empty.files_changed, empty.insertions, empty.deletions),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn unknown_commits_and_folders_without_git_answer_quietly() {
    let repo = checkout(None);
    assert_eq!(
        commit_preview(&repo.path, "abcdef1234567").await,
        CommitLookup::Missing
    );
    // A tree, not a commit.
    let tree = git_sync(&repo.path, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        commit_preview(&repo.path, &tree).await,
        CommitLookup::Missing
    );
    // Malformed input never reaches git.
    assert_eq!(
        commit_preview(&repo.path, "--output=/tmp/x").await,
        CommitLookup::Missing
    );
    let plain = tempfile::tempdir().unwrap();
    assert_eq!(
        commit_preview(plain.path(), &repo.second).await,
        CommitLookup::NoCheckout
    );
    assert_eq!(
        resolve(None, plain.path(), &commit(&repo.second)).await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::NoCheckout,
            url: None
        }
    );
    assert_eq!(
        resolve(
            None,
            plain.path(),
            &GitRef::Issue {
                repo: None,
                number: 4
            }
        )
        .await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::NoCheckout,
            url: None
        }
    );
    assert_eq!(
        resolve(None, &repo.path, &commit("abcdef1234567")).await,
        GitRefResolution::NotFound { url: None }
    );
    assert_eq!(
        resolve(
            None,
            &repo.path,
            &GitRef::Issue {
                repo: None,
                number: 4
            }
        )
        .await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::NoGithubRemote,
            url: None
        }
    );
}

#[tokio::test]
async fn github_checkouts_link_their_commits_and_numbers() {
    let repo = checkout(Some("git@github.com:acme/app.git"));
    let GitRefResolution::Commit(found) =
        resolve(None, &repo.path, &commit(&repo.second[..8])).await
    else {
        panic!("expected a commit")
    };
    assert_eq!(
        found.url.as_deref(),
        Some(format!("https://github.com/acme/app/commit/{}", repo.second).as_str()),
        "the link names the full SHA"
    );
    assert_eq!(
        resolve(None, &repo.path, &commit("abcdef1")).await,
        GitRefResolution::NotFound {
            url: Some("https://github.com/acme/app/commit/abcdef1".into())
        }
    );
    // A commit URL of the same repository (any case) is this checkout's.
    let same = GitRef::Commit {
        repo: Some("Acme/App".into()),
        sha: "abcdef1".into(),
    };
    assert!(matches!(
        resolve(None, &repo.path, &same).await,
        GitRefResolution::NotFound { .. }
    ));
    // Another repository's commit: shown when the checkout has it…
    let other = |sha: &str| GitRef::Commit {
        repo: Some("fan/fork".into()),
        sha: sha.into(),
    };
    let GitRefResolution::Commit(found) = resolve(None, &repo.path, &other(&repo.root)).await
    else {
        panic!("expected a commit")
    };
    assert_eq!(
        found.url,
        Some(format!("https://github.com/fan/fork/commit/{}", repo.root))
    );
    // …else unavailable, with its page.
    assert_eq!(
        resolve(None, &repo.path, &other("abcdef1")).await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::OtherRepository,
            url: Some("https://github.com/fan/fork/commit/abcdef1".into())
        }
    );
    // A bare number resolves against the remote; without GitHub on the
    // engine it is still a link.
    assert_eq!(
        resolve(
            None,
            &repo.path,
            &GitRef::Issue {
                repo: None,
                number: 12
            }
        )
        .await,
        GitRefResolution::Unavailable {
            reason: GitRefUnavailable::SignedOut,
            url: Some("https://github.com/acme/app/issues/12".into())
        }
    );
}

#[test]
fn shortstat_lines_parse_in_every_shape() {
    assert_eq!(
        parse_shortstat("\n 3 files changed, 10 insertions(+), 2 deletions(-)\n"),
        (3, 10, 2)
    );
    assert_eq!(
        parse_shortstat(" 1 file changed, 1 insertion(+)"),
        (1, 1, 0)
    );
    assert_eq!(
        parse_shortstat(" 1 file changed, 4 deletions(-)"),
        (1, 0, 4)
    );
    assert_eq!(parse_shortstat(""), (0, 0, 0));
    let parsed = parse_commit("abc\0ab\0Name\x001700000000\0Subject: x\0\n\n 1 file changed\n")
        .expect("parses");
    assert_eq!(parsed.files_changed, 1);
    assert_eq!(parsed.subject, "Subject: x");
    assert_eq!(parse_commit("abc\0ab\0Name\0not-a-time\0S\0"), None);
}
