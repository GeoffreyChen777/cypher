use super::*;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ── a scripted stand-in for github.com + api.github.com ────────────────────

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    auth: Option<String>,
    form: HashMap<String, String>,
}

type Handler = Arc<dyn Fn(&Req) -> (u16, serde_json::Value) + Send + Sync>;

struct MockGithub {
    base: String,
    requests: Arc<Mutex<Vec<Req>>>,
}

impl MockGithub {
    async fn start(
        handler: impl Fn(&Req) -> (u16, serde_json::Value) + Send + Sync + 'static,
    ) -> Self {
        let handler: Handler = Arc::new(handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<Req>>> = Arc::default();
        let log = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let mut lines = head.lines();
                    let mut first = lines.next().unwrap_or_default().split(' ');
                    let method = first.next().unwrap_or_default().to_string();
                    let target = first.next().unwrap_or_default().to_string();
                    let mut length = 0usize;
                    let mut auth = None;
                    for line in lines {
                        if let Some((name, value)) = line.split_once(':') {
                            match name.trim().to_ascii_lowercase().as_str() {
                                "content-length" => length = value.trim().parse().unwrap_or(0),
                                "authorization" => auth = Some(value.trim().to_string()),
                                _ => {}
                            }
                        }
                    }
                    while buf.len() < head_end + length {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                    let url = reqwest::Url::parse(&format!("http://mock{target}")).unwrap();
                    let form = reqwest::Url::parse(&format!("http://mock/?{body}"))
                        .unwrap()
                        .query_pairs()
                        .into_owned()
                        .collect();
                    let req = Req {
                        method,
                        path: url.path().to_string(),
                        query: url.query_pairs().into_owned().collect(),
                        auth,
                        form,
                    };
                    log.lock().unwrap().push(req.clone());
                    let (status, value) = handler(&req);
                    let body = value.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { base, requests }
    }

    fn config(&self, client_id: Option<&str>) -> GithubConfig {
        GithubConfig {
            client_id: client_id.map(str::to_string),
            app_slug: Some("cypher-test".into()),
            api_base: self.base.clone(),
            web_base: self.base.clone(),
            local_fallbacks: false,
            poll_unit: Duration::from_millis(5),
        }
    }

    fn requests(&self, path: &str) -> Vec<Req> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|req| req.path == path)
            .cloned()
            .collect()
    }
}

fn issue_json(number: u64, repo: &str, pr: bool) -> serde_json::Value {
    let mut issue = json!({
        "number": number,
        "title": format!("Issue {number}"),
        "state": "open",
        "labels": [{"name": "bug"}],
        "html_url": format!("https://github.com/{repo}/issues/{number}"),
        "user": {"login": "someone"},
        "body": "body",
        "comments": 0,
        "repository_url": format!("https://api.github.com/repos/{repo}"),
    });
    if pr {
        issue["pull_request"] = json!({"url": "x"});
    }
    issue
}

fn git_checkout(remote: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", remote]] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
    }
    dir
}

fn write_account(dir: &Path, account: &StoredAccount) {
    write_private_atomic(
        &dir.join(ACCOUNT_FILE),
        &serde_json::to_vec(account).unwrap(),
    )
    .unwrap();
}

fn account(token: &str, expires_in: Option<i64>, refresh: Option<&str>) -> StoredAccount {
    let now = now_secs();
    StoredAccount {
        client_id: "Iv1.test".into(),
        login: "octo".into(),
        access_token: token.into(),
        expires_at: expires_in.map(|secs| now + secs),
        refresh_token: refresh.map(str::to_string),
        refresh_expires_at: refresh.map(|_| now + 1_000_000),
    }
}

async fn wait_for_login(github: &Github, login_id: &str) -> GithubLoginPoll {
    for _ in 0..500 {
        let poll = github.poll_login(login_id);
        if poll.state != GithubLoginState::Pending {
            return poll;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("sign-in never finished");
}

// ── sign-in ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn device_flow_signs_in_and_issue_search_uses_the_new_token() {
    let polls = Arc::new(AtomicUsize::new(0));
    let polls_seen = polls.clone();
    let mock = MockGithub::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/login/device/code") => {
            assert_eq!(req.form["client_id"], "Iv1.test");
            (
                200,
                json!({
                    "device_code": "dc-1", "user_code": "ABCD-1234",
                    "verification_uri": "https://github.com/login/device",
                    "expires_in": 900, "interval": 1
                }),
            )
        }
        ("POST", "/login/oauth/access_token") => {
            assert_eq!(req.form["device_code"], "dc-1");
            assert!(!req.form.contains_key("client_secret"));
            match polls_seen.fetch_add(1, Ordering::SeqCst) {
                0 => (200, json!({"error": "authorization_pending"})),
                1 => (200, json!({"error": "slow_down", "interval": 1})),
                _ => (
                    200,
                    json!({
                        "access_token": "ghu_1", "expires_in": 28800,
                        "refresh_token": "ghr_1", "refresh_token_expires_in": 15897600,
                        "token_type": "bearer", "scope": ""
                    }),
                ),
            }
        }
        ("GET", "/user") => (200, json!({"login": "octo"})),
        ("GET", "/repos/o/r") => (200, json!({"full_name": "o/r"})),
        ("GET", "/search/issues") => (
            200,
            json!({"items": [
                issue_json(7, "o/r", false),
                issue_json(8, "o/r", true),
                issue_json(9, "other/repo", false),
            ]}),
        ),
        ("GET", "/repos/o/r/issues") => (
            200,
            json!([
                issue_json(5, "o/r", true),
                issue_json(2, "o/r", false),
                issue_json(7, "o/r", false),
                issue_json(3, "o/r", false),
            ]),
        ),
        _ => (404, json!({"message": "Not Found"})),
    })
    .await;
    let data = tempfile::tempdir().unwrap();
    let github = Github::new(mock.config(Some("Iv1.test")), data.path());

    let start = github.start_login().await.expect("start");
    assert_eq!(start.user_code, "ABCD-1234");
    let done = wait_for_login(&github, &start.login_id).await;
    assert_eq!(done.state, GithubLoginState::Done, "{:?}", done.message);
    assert_eq!(done.login.as_deref(), Some("octo"));
    assert!(polls.load(Ordering::SeqCst) >= 3);

    let file = data.path().join(ACCOUNT_FILE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let stored: StoredAccount = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(stored.refresh_token.as_deref(), Some("ghr_1"));

    let status = github.status().await;
    assert_eq!(status.login.as_deref(), Some("octo"));
    assert!(status.sign_in_available);
    assert_eq!(
        status.install_url.as_deref(),
        Some(format!("{}/apps/cypher-test/installations/new", mock.base).as_str())
    );

    let checkout = git_checkout("git@github.com:o/r.git");
    let search = github
        .search_issues(checkout.path(), "")
        .await
        .expect("search");
    let GithubIssueSearch::Ok { repo, issues } = search else {
        panic!("{search:?}")
    };
    assert_eq!(repo, "o/r");
    let numbers: Vec<u64> = issues.iter().map(|issue| issue.number).collect();
    assert_eq!(
        numbers,
        [7, 2, 3],
        "assigned first, PRs and other repos dropped"
    );
    assert!(issues[0].assigned_to_me && !issues[1].assigned_to_me);
    let searches = mock.requests("/search/issues");
    assert!(searches[0].query["q"].contains("repo:o/r is:issue is:open assignee:@me"));
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|req| req.method == "GET")
            .all(|req| req.auth.as_deref() == Some("Bearer ghu_1"))
    );
}

#[tokio::test]
async fn declined_and_unconfigured_sign_ins_report_why() {
    let mock = MockGithub::start(|req| match req.path.as_str() {
        "/login/device/code" => (
            200,
            json!({
                "device_code": "dc", "user_code": "C", "verification_uri": "v",
                "expires_in": 900, "interval": 1
            }),
        ),
        _ => (200, json!({"error": "access_denied"})),
    })
    .await;
    let data = tempfile::tempdir().unwrap();
    let github = Github::new(mock.config(Some("Iv1.test")), data.path());
    let start = github.start_login().await.unwrap();
    let poll = wait_for_login(&github, &start.login_id).await;
    assert_eq!(poll.state, GithubLoginState::Error);
    assert!(poll.message.unwrap().contains("cancelled"));
    assert!(!data.path().join(ACCOUNT_FILE).exists());

    let unconfigured = Github::new(mock.config(None), data.path());
    let err = unconfigured.start_login().await.unwrap_err();
    assert!(err.to_string().contains("isn't set up"), "{err}");
    assert!(!unconfigured.status().await.sign_in_available);
}

#[tokio::test]
async fn expiring_tokens_refresh_without_a_secret_and_rotate() {
    let mock = MockGithub::start(|req| match req.path.as_str() {
        "/login/oauth/access_token" => {
            assert_eq!(req.form["grant_type"], "refresh_token");
            assert!(!req.form.contains_key("client_secret"));
            match req.form["refresh_token"].as_str() {
                "ghr_1" => (
                    200,
                    json!({
                        "access_token": "ghu_2", "expires_in": 28800,
                        "refresh_token": "ghr_2", "refresh_token_expires_in": 15897600
                    }),
                ),
                _ => (200, json!({"error": "bad_refresh_token"})),
            }
        }
        _ => (404, json!({})),
    })
    .await;

    let data = tempfile::tempdir().unwrap();
    write_account(data.path(), &account("ghu_1", Some(60), Some("ghr_1")));
    let github = Github::new(mock.config(Some("Iv1.test")), data.path());
    let (credential, login) = github.cypher_credential().await.expect("refreshed");
    assert_eq!(
        (credential.token.as_str(), login.as_str()),
        ("ghu_2", "octo")
    );
    let stored: StoredAccount =
        serde_json::from_slice(&std::fs::read(data.path().join(ACCOUNT_FILE)).unwrap()).unwrap();
    assert_eq!(stored.refresh_token.as_deref(), Some("ghr_2"));
    // Fresh now: no second refresh.
    github.cypher_credential().await.unwrap();
    assert_eq!(mock.requests("/login/oauth/access_token").len(), 1);

    let rejected = tempfile::tempdir().unwrap();
    write_account(
        rejected.path(),
        &account("ghu_x", Some(60), Some("ghr_revoked")),
    );
    let github = Github::new(mock.config(Some("Iv1.test")), rejected.path());
    assert!(github.cypher_credential().await.is_none());
    assert!(
        !rejected.path().join(ACCOUNT_FILE).exists(),
        "rejected refresh signs out"
    );

    let dead = tempfile::tempdir().unwrap();
    write_account(dead.path(), &account("ghu_y", Some(-10), None));
    let github = Github::new(mock.config(Some("Iv1.test")), dead.path());
    assert!(github.cypher_credential().await.is_none());
}

#[tokio::test]
async fn repositories_without_access_or_credentials_explain_themselves() {
    let mock = MockGithub::start(|req| match req.path.as_str() {
        "/repos/o/private" => (404, json!({"message": "Not Found"})),
        _ => (404, json!({})),
    })
    .await;
    let checkout = git_checkout("https://github.com/o/private.git");

    let signed_out = tempfile::tempdir().unwrap();
    let github = Github::new(mock.config(Some("Iv1.test")), signed_out.path());
    assert_eq!(
        github.search_issues(checkout.path(), "x").await.unwrap(),
        GithubIssueSearch::Unavailable {
            reason: GithubUnavailable::SignedOut,
            repo: Some("o/private".into()),
            install_url: None,
        }
    );

    let signed_in = tempfile::tempdir().unwrap();
    write_account(signed_in.path(), &account("ghu_1", None, None));
    let github = Github::new(mock.config(Some("Iv1.test")), signed_in.path());
    let GithubIssueSearch::Unavailable {
        reason,
        install_url,
        ..
    } = github.search_issues(checkout.path(), "").await.unwrap()
    else {
        panic!("expected unavailable")
    };
    assert_eq!(reason, GithubUnavailable::NoAccess);
    assert!(
        install_url
            .unwrap()
            .ends_with("/apps/cypher-test/installations/new")
    );

    let plain = git_checkout("https://gitlab.com/o/r.git");
    assert!(matches!(
        github.search_issues(plain.path(), "").await.unwrap(),
        GithubIssueSearch::Unavailable {
            reason: GithubUnavailable::NoGithubRemote,
            ..
        }
    ));
}

#[tokio::test]
async fn snapshots_read_the_newest_comment_pages() {
    let mock = MockGithub::start(|req| match req.path.as_str() {
        "/repos/o/r" => (200, json!({})),
        "/repos/o/r/issues/4" => {
            let mut issue = issue_json(4, "o/r", false);
            issue["comments"] = json!(205);
            issue["body"] = serde_json::Value::Null;
            (200, issue)
        }
        "/repos/o/r/issues/4/comments" => {
            let page: usize = req.query["page"].parse().unwrap();
            let count = if page == 3 { 5 } else { 100 };
            let comments: Vec<_> = (0..count)
                .map(|ix| {
                    let n = (page - 1) * 100 + ix;
                    json!({"user": {"login": format!("u{n}")}, "body": format!("c{n}"),
                           "created_at": format!("t{n:04}")})
                })
                .collect();
            (200, json!(comments))
        }
        _ => (404, json!({})),
    })
    .await;
    let data = tempfile::tempdir().unwrap();
    write_account(data.path(), &account("ghu_1", None, None));
    let github = Github::new(mock.config(Some("Iv1.test")), data.path());
    let snapshot = github.issue_snapshot("o/r", 4).await.expect("snapshot");
    let pages: Vec<String> = mock
        .requests("/repos/o/r/issues/4/comments")
        .into_iter()
        .map(|req| req.query["page"].clone())
        .collect();
    assert_eq!(
        pages,
        ["2", "3"],
        "short last page topped up from the one before"
    );
    assert_eq!(snapshot.comments.len(), 105);
    assert_eq!(snapshot.omitted_comments, 100);
    assert_eq!(snapshot.comments.last().unwrap().author, "u204");
    assert_eq!(snapshot.body, "");
    assert_eq!(snapshot.url, "https://github.com/o/r/issues/4");

    let missing = github.issue_snapshot("o/r", 99).await.unwrap_err();
    assert!(missing.to_string().contains("doesn't exist"), "{missing}");
    assert!(github.issue_snapshot("../x", 1).await.is_err());
}

// ── pure helpers ───────────────────────────────────────────────────────────

#[test]
fn the_cypher_app_is_the_default_and_env_can_override_or_disable_it() {
    // SAFETY: tests in this module don't read these variables concurrently.
    unsafe { std::env::remove_var("CYPHER_GITHUB_CLIENT_ID") };
    assert_eq!(
        GithubConfig::detect().client_id.as_deref(),
        Some(DEFAULT_GITHUB_CLIENT_ID)
    );
    unsafe { std::env::set_var("CYPHER_GITHUB_CLIENT_ID", "Iv1.other") };
    assert_eq!(
        GithubConfig::detect().client_id.as_deref(),
        Some("Iv1.other")
    );
    unsafe { std::env::set_var("CYPHER_GITHUB_CLIENT_ID", "") };
    assert_eq!(GithubConfig::detect().client_id, None);
    unsafe { std::env::remove_var("CYPHER_GITHUB_CLIENT_ID") };
}

#[test]
fn github_urls_parse_in_every_remote_form() {
    for url in [
        "https://github.com/GeoffreyChen777/cypher.git",
        "https://github.com/GeoffreyChen777/cypher",
        "https://user:token@github.com/GeoffreyChen777/cypher.git/",
        "git@github.com:GeoffreyChen777/cypher.git",
        "ssh://git@github.com/GeoffreyChen777/cypher.git",
        "ssh://git@ssh.github.com:443/GeoffreyChen777/cypher.git",
        "git://github.com/GeoffreyChen777/cypher",
    ] {
        assert_eq!(
            parse_github_url(url).as_deref(),
            Some("GeoffreyChen777/cypher"),
            "{url}"
        );
    }
}

#[test]
fn non_github_or_malformed_urls_are_rejected() {
    for url in [
        "https://gitlab.com/owner/repo.git",
        "git@bitbucket.org:owner/repo.git",
        "https://github.com/owner",
        "https://github.com/owner/repo/extra",
        "https://github.com/../repo",
        "https://github.com/owner/-repo",
        "/Users/me/src/repo",
        "",
    ] {
        assert_eq!(parse_github_url(url), None, "{url}");
    }
}

#[test]
fn repo_pick_honors_set_default_then_remote_names() {
    let config = "remote.fork.url git@github.com:me/cypher.git\n\
                  remote.origin.url https://github.com/me/cypher-origin.git\n\
                  remote.upstream.url https://github.com/org/cypher.git\n";
    assert_eq!(pick_github_repo(config).as_deref(), Some("org/cypher"));

    let with_default = format!("{config}remote.fork.gh-resolved base\n");
    assert_eq!(
        pick_github_repo(&with_default).as_deref(),
        Some("me/cypher")
    );

    let explicit = format!("{config}remote.fork.gh-resolved other/repo\n");
    assert_eq!(pick_github_repo(&explicit).as_deref(), Some("other/repo"));

    let first_github = "remote.work.url https://gitlab.com/a/b.git\n\
                        remote.mirror.url git@github.com:a/b.git\n";
    assert_eq!(pick_github_repo(first_github).as_deref(), Some("a/b"));
    assert_eq!(
        pick_github_repo("remote.origin.url https://gitlab.com/a/b.git\n"),
        None
    );
}

#[test]
fn repo_argument_validation() {
    assert!(valid_repo("GeoffreyChen777/cypher"));
    assert!(valid_repo("a-b/c_d.e"));
    for bad in [
        "", "a", "a/b/c", "a/", "/b", "a/..", "-a/b", "a b/c", "a/b;rm",
    ] {
        assert!(!valid_repo(bad), "{bad}");
    }
}

#[test]
fn refresh_timing() {
    let now = now_secs();
    let soon = account("t", Some(60), Some("r"));
    assert!(soon.needs_refresh(now) && !soon.expired(now));
    assert!(!account("t", Some(3600), Some("r")).needs_refresh(now));
    assert!(!account("t", None, None).needs_refresh(now));
    let unrefreshable = account("t", Some(60), None);
    assert!(!unrefreshable.needs_refresh(now));
    assert!(account("t", Some(-1), None).expired(now));
    let mut stale_refresh = account("t", Some(60), Some("r"));
    stale_refresh.refresh_expires_at = Some(now - 1);
    assert!(!stale_refresh.needs_refresh(now));
}

#[test]
fn credentials_never_print_their_token() {
    let credential = Credential {
        source: GithubCredentialSource::GhCli,
        token: "gho_secret".into(),
    };
    assert!(!format!("{credential:?}").contains("gho_secret"));
    assert_eq!(plausible_token(" gho_x \n").as_deref(), Some("gho_x"));
    assert_eq!(plausible_token("two words"), None);
    assert_eq!(plausible_token(""), None);
}

fn rest(number: u64) -> RestIssue {
    serde_json::from_value(issue_json(number, "o/r", false)).unwrap()
}

#[test]
fn merge_puts_assigned_then_exact_first_and_dedupes() {
    let rows = merge_issues(
        vec![rest(3)],
        vec![rest(7)],
        vec![rest(9), rest(3), rest(7), rest(1)],
    );
    let numbers: Vec<u64> = rows.iter().map(|row| row.number).collect();
    assert_eq!(numbers, [3, 7, 9, 1]);
    assert!(rows[0].assigned_to_me);
    assert!(!rows[1].assigned_to_me);
    assert_eq!(rows[0].labels, ["bug"]);
}

#[test]
fn snapshot_keeps_newest_comments_within_budget() {
    let mut big = rest(5);
    big.body = Some("b".repeat(BODY_MAX_CHARS + 10));
    big.user = None;
    let comments = (0..20)
        .map(|ix| RestComment {
            user: Some(RestUser {
                login: format!("u{ix}"),
            }),
            body: Some("c".repeat(COMMENT_MAX_CHARS + 100)),
            created_at: format!("2026-01-{:02}", ix + 1),
        })
        .collect();
    let snapshot = snapshot_from("o/r", big, comments, 20);
    assert!(snapshot.body.ends_with("… [truncated]"));
    let kept = snapshot.comments.len();
    assert!(kept > 0 && kept < 20);
    assert_eq!(snapshot.omitted_comments, 20 - kept);
    assert_eq!(snapshot.comments.last().unwrap().author, "u19");
    assert!(snapshot.comments[0].created_at < snapshot.comments[kept - 1].created_at);
    assert_eq!(snapshot.author, "ghost");
    let used: usize = snapshot
        .comments
        .iter()
        .map(|comment| comment.body.chars().count())
        .sum();
    assert!(used <= COMMENTS_BUDGET_CHARS);
}

#[test]
fn search_reply_serializes_with_a_status_tag() {
    let ok = serde_json::to_value(GithubIssueSearch::Ok {
        repo: "o/r".into(),
        issues: Vec::new(),
    })
    .unwrap();
    assert_eq!(ok["status"], "ok");
    let unavailable = serde_json::to_value(GithubIssueSearch::Unavailable {
        reason: GithubUnavailable::NoAccess,
        repo: Some("o/r".into()),
        install_url: Some("u".into()),
    })
    .unwrap();
    assert_eq!(unavailable["status"], "unavailable");
    assert_eq!(unavailable["reason"], "noAccess");
    assert_eq!(unavailable["installUrl"], "u");
}

/// Live check through this Mac's own credentials (`gh`, then git's helper)
/// against a real repository:
/// `GH_LIVE_REMOTE=git@github.com:cli/cli.git cargo test -p cypher-engine github_live -- --ignored`.
#[tokio::test]
#[ignore]
async fn github_live_search_and_snapshot_with_local_credentials() {
    let remote =
        std::env::var("GH_LIVE_REMOTE").unwrap_or_else(|_| "git@github.com:cli/cli.git".into());
    let checkout = git_checkout(&remote);
    let data = tempfile::tempdir().unwrap();
    let mut config = GithubConfig::detect();
    config.client_id = None;
    let github = Github::new(config, data.path());
    let status = github.status().await;
    eprintln!("status: {status:?}");
    let search = github
        .search_issues(checkout.path(), "")
        .await
        .expect("search");
    let GithubIssueSearch::Ok { repo, issues } = search else {
        panic!("{search:?}")
    };
    eprintln!("{} rows, first: {:?}", issues.len(), issues.first());
    let first = issues.first().expect("an open issue");
    let exact = github
        .search_issues(checkout.path(), &first.number.to_string())
        .await
        .expect("exact");
    let GithubIssueSearch::Ok { issues: exact, .. } = exact else {
        panic!("{exact:?}")
    };
    assert_eq!(exact.first().map(|issue| issue.number), Some(first.number));
    let snapshot = github
        .issue_snapshot(&repo, first.number)
        .await
        .expect("snapshot");
    eprintln!(
        "{} — {} comments kept, {} omitted",
        snapshot.title,
        snapshot.comments.len(),
        snapshot.omitted_comments
    );
}

/// Live check of the git-credential fallback alone (no `gh`):
/// `cargo test -p cypher-engine github_live_git_credential -- --ignored`.
#[tokio::test]
#[ignore]
async fn github_live_git_credential_reads_the_api() {
    let token = git_credential(None).await.expect("a github.com credential");
    let data = tempfile::tempdir().unwrap();
    let github = Github::new(GithubConfig::detect(), data.path());
    let user: RestUser = github.get(&token, "/user", &[]).await.expect("GET /user");
    eprintln!("git credential authenticates as {}", user.login);
}
