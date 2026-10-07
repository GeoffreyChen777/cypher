//! Loopback release fixtures; no real releases or user installation touched.
use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Routes = BTreeMap<String, (u16, Vec<u8>)>;

/// This platform's own release channel — the path a client reaches for
/// first now that platforms publish independently.
fn channel_manifest() -> String {
    format!("/releases/{}/manifest.json", channel())
}

struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    routes: Arc<std::sync::Mutex<Routes>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(routes: Vec<(&str, u16, Vec<u8>)>) -> Server {
    let routes: Routes = routes
        .into_iter()
        .map(|(path, status, body)| (path.into(), (status, body)))
        .collect();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::new(std::sync::Mutex::new(routes));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_routes = routes.clone();
    let server_requests = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let n = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..n]);
            let path = request.split_whitespace().nth(1).unwrap_or("");
            server_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (status, body) = server_routes
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .unwrap_or((404, vec![]));
            let header = format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        }
    });
    Server {
        url,
        task,
        requests,
        routes,
    }
}

#[tokio::test]
async fn returning_to_the_app_checks_once_then_holds_off() {
    use std::sync::atomic::Ordering::SeqCst;
    let body = br#"{"version":"0.0.0","build":1,"files":{}}"#.to_vec();
    let server = server(vec![(&channel_manifest(), 200, body)]).await;
    let dir = tempfile::tempdir().unwrap();
    let updater = Updater::spawn(server.url.clone(), None, dir.path().into());
    let mut status = updater.watch();

    // Coming back to the app surfaces a release published while away, instead
    // of waiting out the polling cadence.
    assert!(updater.check_on_activation(), "first activation must check");
    tokio::time::timeout(
        Duration::from_secs(3),
        status.wait_for(|s| s.checked_at.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    let after_first = server.requests.load(SeqCst);
    assert!(after_first >= 1);

    // Alt-tabbing must not turn into a request per focus change.
    for _ in 0..50 {
        assert!(
            !updater.check_on_activation(),
            "activations inside the cooldown must not check"
        );
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        server.requests.load(SeqCst),
        after_first,
        "cooldown must hold the endpoint still"
    );
    updater.shutdown().await;
}

#[tokio::test]
async fn auth_rotation_does_not_poll_releases_but_recovery_still_retries() {
    use std::sync::atomic::Ordering::SeqCst;
    let body = br#"{"version":"0.0.0","files":{}}"#.to_vec();
    let server = server(vec![(&channel_manifest(), 200, body.clone())]).await;
    let dir = tempfile::tempdir().unwrap();
    let updater = Updater::spawn(server.url.clone(), None, dir.path().into());
    let mut status = updater.watch();

    // Rotation during startup leaves the existing initial-check timer alone.
    updater.check_after_auth_change(true, false);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(server.requests.load(SeqCst), 0);

    updater.check_after_auth_change(true, true);
    tokio::time::timeout(
        Duration::from_secs(3),
        status.wait_for(|s| s.checked_at.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(server.requests.load(SeqCst), 1);
    for _ in 0..100 {
        updater.check_after_auth_change(true, false);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        server.requests.load(SeqCst),
        1,
        "healthy rotations must not fetch"
    );

    // A failed public check can recover early when fresh credentials indicate
    // the network is back, even if AuthState stayed SignedIn during the outage.
    server
        .routes
        .lock()
        .unwrap()
        .insert(channel_manifest(), (503, vec![]));
    updater.check_now();
    tokio::time::timeout(
        Duration::from_secs(3),
        status.wait_for(|s| s.error.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(server.requests.load(SeqCst), 2);
    updater.check_after_auth_change(false, false);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        server.requests.load(SeqCst),
        2,
        "sign-out is not connectivity recovery"
    );

    server
        .routes
        .lock()
        .unwrap()
        .insert(channel_manifest(), (200, body));
    updater.check_after_auth_change(true, false);
    tokio::time::timeout(
        Duration::from_secs(3),
        status.wait_for(|s| s.error.is_none()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(server.requests.load(SeqCst), 3);
    updater.check_after_auth_change(true, false);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(server.requests.load(SeqCst), 3);
    updater.shutdown().await;
}

#[tokio::test]
async fn check_fetches_the_manifest_immediately() {
    use std::sync::atomic::Ordering::SeqCst;
    let body = br#"{"version":"9.9.9","files":{}}"#.to_vec();
    let server = server(vec![(&channel_manifest(), 200, body)]).await;
    let dir = tempfile::tempdir().unwrap();
    let updater = Updater::spawn(server.url.clone(), None, dir.path().into());
    let status = updater.check().await;
    assert_eq!(status.latest_version.as_deref(), Some("9.9.9"));
    assert!(status.update_available);
    assert!(status.checked_at.is_some());
    assert!(status.error.is_none());
    assert!(server.requests.load(SeqCst) >= 1);
    updater.shutdown().await;
}

fn manifest(file: &str, bytes: &[u8]) -> Manifest {
    Manifest {
        version: "1.2.3".into(),
        files: BTreeMap::from([(
            file.into(),
            FileMeta {
                sha256: Some(format!("{:x}", Sha256::digest(bytes))),
            },
        )]),
        ..Manifest::default()
    }
}

#[test]
fn release_paths_reject_traversal_and_malformed_versions() {
    for bad in [
        "",
        ".",
        "..",
        "../outside",
        "/tmp/x",
        "1/../../x",
        "1..2",
        " 1.2",
        "v1.2",
        "1\n2",
        "1?x",
    ] {
        assert!(validate_version(bad).is_err(), "{bad}");
    }
    for good in ["0.2.2", "0.85.0.4", "1"] {
        validate_version(good).unwrap();
    }
}

#[tokio::test]
async fn bad_manifest_must_not_downgrade_to_latest_txt() {
    for (code, body) in [
        (500, b"unavailable".to_vec()),
        (200, br#"{"version":"../../outside"}"#.to_vec()),
        (200, b"not json".to_vec()),
    ] {
        let shared = server(vec![
            ("/releases/manifest.json", code, body.clone()),
            ("/releases/latest.txt", 200, b"1.2.3".to_vec()),
        ])
        .await;
        assert!(fetch_latest(&shared.url).await.is_err());

        // A broken per-platform channel must not quietly fall through to the
        // shared one either: that would update from a channel this platform
        // does not publish to.
        let own = server(vec![
            (&channel_manifest(), code, body),
            (
                "/releases/manifest.json",
                200,
                br#"{"version":"9.9.9","files":{}}"#.to_vec(),
            ),
            ("/releases/latest.txt", 200, b"9.9.9".to_vec()),
        ])
        .await;
        assert!(fetch_latest(&own.url).await.is_err());
    }
}

#[tokio::test]
async fn the_platform_channel_wins_over_the_shared_one() {
    let server = server(vec![
        (
            &channel_manifest(),
            200,
            br#"{"version":"2.0.0","build":3,"files":{},"roles":{}}"#.to_vec(),
        ),
        (
            "/releases/manifest.json",
            200,
            br#"{"version":"1.0.0","files":{}}"#.to_vec(),
        ),
    ])
    .await;
    let release = fetch_latest(&server.url).await.unwrap();
    assert_eq!(release.version, "2.0.0");
    assert_eq!(release.build, 3);
}

#[tokio::test]
async fn a_recut_build_downloads_the_name_the_manifest_states() {
    // Build 2 does not use the historic name, so a client that rebuilt the
    // name from the version alone would 404. The role mapping is what makes a
    // per-platform re-cut reachable.
    let (_, arch) = platform_key();
    let file = format!("cypher-1.2.3-b2-linux-{arch}.tar.gz");
    let manifest = Manifest {
        version: "1.2.3".into(),
        build: 2,
        roles: BTreeMap::from([(format!("headless-{arch}"), file.clone())]),
        ..Manifest::default()
    };
    assert_eq!(manifest.headless_file(), file);
    assert_ne!(manifest.headless_file(), headless_artifact("1.2.3"));

    // Without a role mapping the historic name is still what legacy channels
    // resolve to.
    let legacy = Manifest {
        version: "1.2.3".into(),
        ..Manifest::default()
    };
    assert_eq!(legacy.headless_file(), headless_artifact("1.2.3"));
}

#[tokio::test]
async fn legacy_pointer_still_requires_a_standalone_checksum() {
    let file = "cypher-1.2.3-linux-x86_64.tar.gz";
    let bytes = b"verified download";
    let hash = format!("{:x}\n", Sha256::digest(bytes));
    let artifact_path = format!("/releases/{file}");
    let checksum_path = format!("{artifact_path}.sha256");
    let server = server(vec![
        ("/releases/latest.txt", 200, b"1.2.3\n".to_vec()),
        (&artifact_path, 200, bytes.to_vec()),
        (&checksum_path, 200, hash.into_bytes()),
    ])
    .await;
    let release = fetch_latest(&server.url).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(file);
    download_release_file(&server.url, &release, file, &dest)
        .await
        .unwrap();
    assert_eq!(std::fs::read(dest).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn checksum_failure_preserves_existing_file_and_cleans_partial() {
    let file = "cypher-1.2.3-linux-x86_64.tar.gz";
    let path = format!("/releases/{file}");
    let server = server(vec![(&path, 200, b"bad bytes".to_vec())]).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("archive");
    std::fs::write(&dest, b"old bytes").unwrap();
    let mut release = manifest(file, b"correct bytes");
    assert!(
        download_release_file(&server.url, &release, file, &dest)
            .await
            .is_err()
    );
    release.files.clear();
    assert!(
        download_release_file(&server.url, &release, file, &dest)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(dest).unwrap(), b"old bytes");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn partial_manifest_cannot_silently_skip_platform_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let mut release = manifest("other-platform", b"correct");
    for hash in [None, Some("invalid".into())] {
        release
            .files
            .insert("artifact".into(), FileMeta { sha256: hash });
        let error = download_release_file(
            "http://127.0.0.1:1",
            &release,
            "artifact",
            &dir.path().join("out"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("checksum") || error.to_string().contains("SHA-256"));
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[cfg(unix)]
fn archive(symlink: bool) -> (tempfile::TempDir, PathBuf, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = headless_artifact("1.2.3");
    let root = file.trim_end_matches(".tar.gz").to_owned();
    let contents = dir.path().join(&root);
    std::fs::create_dir(&contents).unwrap();
    if symlink {
        std::os::unix::fs::symlink("/bin/sh", contents.join("cypher")).unwrap();
    } else {
        std::fs::write(contents.join("cypher"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            contents.join("cypher"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let tarball = dir.path().join(&file);
    run(
        "tar",
        &[
            "-czf",
            tarball.to_str().unwrap(),
            "-C",
            dir.path().to_str().unwrap(),
            &root,
        ],
    )
    .unwrap();
    (dir, tarball, file)
}

#[cfg(unix)]
#[tokio::test]
async fn headless_download_stages_then_atomically_activates() {
    let (_source, tarball, file) = archive(false);
    let bytes = std::fs::read(tarball).unwrap();
    let release = manifest(&file, &bytes);
    let path = format!("/releases/{file}");
    let server = server(vec![(&path, 200, bytes)]).await;
    let dest = tempfile::tempdir().unwrap();
    stage_headless(&server.url, &release, dest.path())
        .await
        .unwrap();
    assert!(!dest.path().join("current").exists());
    apply_headless(dest.path(), "1.2.3").unwrap();
    assert_eq!(
        std::fs::read_link(dest.path().join("current")).unwrap(),
        dest.path().join("1.2.3")
    );
    // The cache is reusable offline and leaves no temporary staging dirs.
    stage_headless("http://127.0.0.1:1", &release, dest.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(dest.path()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn archive_links_and_broken_binaries_cannot_be_activated() {
    let (_source, tarball, file) = archive(true);
    assert!(validate_headless_archive(&tarball, file.trim_end_matches(".tar.gz")).is_err());
    let dest = tempfile::tempdir().unwrap();
    std::fs::create_dir(dest.path().join("1.2.3")).unwrap();
    std::fs::write(dest.path().join("1.2.3/cypher"), "not executable").unwrap();
    assert!(apply_headless(dest.path(), "1.2.3").is_err());
    assert!(!dest.path().join("current").exists());
    assert!(apply_headless(dest.path(), "../../outside").is_err());
}

#[cfg(unix)]
#[test]
fn binary_probe_has_a_deadline_and_reaps_the_child() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("cypher");
    std::fs::write(&binary, "#!/bin/sh\nexec sleep 60\n").unwrap();
    std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let start = std::time::Instant::now();
    assert!(!headless_binary_ready(dir.path()));
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn staged_update_pruning_keeps_newer_bundles_and_unrelated_entries() {
    let dir = tempfile::tempdir().unwrap();
    let updates = dir.path().join("updates");
    for name in ["0.3.9", "0.3.10", "0.3.11", "notes"] {
        std::fs::create_dir_all(updates.join(name).join("Cypher.app")).unwrap();
    }
    // Startup on 0.3.10: its own bundle and older ones are done with.
    prune_staged_updates(dir.path(), |staged| !version_newer(staged, "0.3.10"));
    let mut left: Vec<_> = std::fs::read_dir(&updates)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["0.3.11", "notes"]);
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn managed_pruning_keeps_current_newer_and_unrelated_entries() {
    let dir = tempfile::tempdir().unwrap();
    // Not named `app`: service units on the test machine cannot pin these.
    let root = dir.path().join("installs");
    for name in ["0.3.1", "0.3.2", "0.3.3", "0.3.4", "notes"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    std::os::unix::fs::symlink(root.join("0.3.2"), root.join("current")).unwrap();
    let crashed = root.join(".stage-crashed");
    let installing = root.join(".install-running");
    std::fs::create_dir_all(&crashed).unwrap();
    std::fs::create_dir_all(&installing).unwrap();
    let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
    std::fs::File::open(&crashed)
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();

    // Startup on 0.3.3 while `current` still names 0.3.2 (not yet restarted).
    prune_managed_versions(&root, |installed| !version_newer(installed, "0.3.3"));
    assert_eq!(
        dir_names(&root),
        [".install-running", "0.3.2", "0.3.4", "current", "notes"],
        "0.3.3 is gone only because nothing runs it in this test"
    );
    assert!(root.join("current").is_symlink());
}

#[test]
fn explicitly_kept_versions_survive_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("installs");
    for name in ["0.3.1", "0.3.2"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    let keep = std::collections::BTreeSet::from(["0.3.1".to_string()]);
    prune_versions_except(&root, &keep, |_| true);
    assert_eq!(dir_names(&root), ["0.3.1"]);
}

#[test]
fn service_units_pin_the_versions_they_run_by_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    for name in ["0.3.1", "0.3.2", "0.3.3"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    let units = dir.path().join("units");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(
        units.join("cypher.service"),
        "[Service]\nExecStart=:\"%h/.cypher/app/0.3.1/cypher\" headless\n",
    )
    .unwrap();
    std::fs::write(
        units.join("cypher-work.service"),
        "[Service]\nExecStart=:\"%h/.cypher/app/current/cypher\" headless\n",
    )
    .unwrap();
    assert_eq!(
        service_pinned_versions(&root, &[units, dir.path().join("missing")]),
        std::collections::BTreeSet::from(["0.3.1".to_string()])
    );
}

#[cfg(target_os = "linux")]
#[test]
fn versions_another_process_runs_from_are_never_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("installs");
    for name in ["0.3.1", "0.3.2"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    // An old desktop app still running a superseded version.
    let binary = root.join("0.3.1").join("cypher");
    std::fs::copy("/bin/sleep", &binary).unwrap();
    let mut child = std::process::Command::new(&binary)
        .arg("30")
        .spawn()
        .unwrap();
    // `/proc/<pid>/exe` names the new image once exec has completed.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !running_versions(&root).contains("0.3.1") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    prune_managed_versions(&root, |_| true);
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(dir_names(&root), ["0.3.1"]);
}

#[test]
fn version_compare() {
    assert!(version_newer("0.1.1", "0.1.0"));
    assert!(version_newer("0.2.0", "0.1.9"));
    assert!(version_newer("0.1.10", "0.1.9"));
    assert!(version_newer("v0.1.1", "0.1.0"));
    assert!(version_newer("0.1.0.1", "0.1.0"));
    assert!(!version_newer("0.1.0", "0.1.0"));
    assert!(!version_newer("0.1.0", "0.1.1"));
    // Garbage never counts as newer.
    assert!(!version_newer("", "0.1.0"));
    assert!(!version_newer("nightly", "0.1.0"));
}

#[test]
fn install_kind_detection() {
    // Cypher install layout.
    assert_eq!(
        detect_install_from(
            Path::new("/home/u/.cypher/app/0.1.1/cypher"),
            Some(Path::new("/home/u")),
        ),
        InstallKind::Managed {
            app_root: PathBuf::from("/home/u/.cypher/app")
        }
    );
    assert_eq!(
        detect_install_from(
            Path::new("/Applications/Cypher.app/Contents/MacOS/cypher"),
            Some(Path::new("/Users/u")),
        ),
        InstallKind::MacApp {
            bundle: PathBuf::from("/Applications/Cypher.app")
        }
    );
    // A path merely containing `.app` without the bundle layout is not a bundle.
    assert_eq!(
        detect_install_from(Path::new("/tmp/foo.app/cypher"), None),
        InstallKind::Unmanaged
    );
    assert_eq!(
        detect_install_from(
            Path::new("/src/target/release/cypher"),
            Some(Path::new("/home/u"))
        ),
        InstallKind::Unmanaged
    );
}

#[test]
fn artifact_names_match_packaging() {
    let (os, arch) = platform_key();
    assert!(headless_artifact("0.2.0").starts_with("cypher-0.2.0-"));
    assert_eq!(
        headless_artifact("0.2.0"),
        format!("cypher-0.2.0-{os}-{arch}.tar.gz")
    );
    assert!(mac_app_artifact("0.2.0").ends_with("-app.tar.gz"));
}

#[test]
fn manifest_parses_with_and_without_files() {
    let full: Manifest = serde_json::from_str(
        r#"{"version":"0.1.1","files":{"cypher-0.1.1-linux-x86_64.tar.gz":{"sha256":"abc"}}}"#,
    )
    .unwrap();
    assert_eq!(full.version, "0.1.1");
    assert_eq!(
        full.files["cypher-0.1.1-linux-x86_64.tar.gz"]
            .sha256
            .as_deref(),
        Some("abc")
    );
    let bare: Manifest = serde_json::from_str(r#"{"version":"0.1.1"}"#).unwrap();
    assert!(bare.files.is_empty());
}

#[tokio::test]
async fn immediate_shutdown_cannot_miss_the_loop_receiver() {
    let updater = Updater::spawn(
        "http://127.0.0.1:1".into(),
        None,
        std::env::temp_dir().join("cypher-update-test"),
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), updater.shutdown())
        .await
        .expect("immediate updater shutdown must not hang");
}

#[test]
fn auto_update_defaults_on_for_linux_only_and_honours_explicit_values() {
    for value in ["1", "true", "YES", " on "] {
        assert!(auto_update_setting(Some(value)), "{value}");
    }
    for value in ["0", "false", "no", "off", "maybe"] {
        assert!(!auto_update_setting(Some(value)), "{value}");
    }
    assert_eq!(auto_update_setting(None), cfg!(target_os = "linux"));
}

#[test]
fn exec_start_rewrite_targets_only_the_matching_line() {
    let text =
        "[Service]\nEnvironment=\"A=1\"\nExecStart=:\"/opt/cypher\" headless\nRestart=on-failure\n";
    assert_eq!(
        exec_line_binary("ExecStart=:\"/opt/cypher\" headless").as_deref(),
        Some("/opt/cypher")
    );
    assert_eq!(
        exec_line_binary("ExecStart=:\"/home/u/bin %% \\\" q/cypher\" headless").as_deref(),
        Some("/home/u/bin % \" q/cypher")
    );
    // Round-trips the daemon installer's quoting, including `%`.
    let odd = "/home/u/dir % \" x/cypher";
    assert_eq!(
        exec_line_binary(&format!("ExecStart=:{} headless", systemd_quote(odd))).as_deref(),
        Some(odd)
    );
    assert_eq!(systemd_quote("a%b"), "\"a%%b\"");
    let rewritten = rewrite_exec_start(text, |line| {
        exec_line_binary(line).as_deref() == Some("/opt/cypher")
    })
    .unwrap();
    assert!(rewritten.contains(CURRENT_EXEC_LINE));
    assert!(rewritten.contains("Environment=\"A=1\""));
    assert!(rewrite_exec_start(text, |line| line.contains("elsewhere")).is_none());
    assert!(
        rewrite_exec_start(&format!("{CURRENT_EXEC_LINE}\n"), |line| {
            line.starts_with("ExecStart=:\"%h/.cypher/app/") && line != CURRENT_EXEC_LINE
        })
        .is_none(),
        "an already-migrated unit is left alone"
    );
}

#[cfg(unix)]
#[test]
fn layout_detection_and_command_link() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("elsewhere").join("app");
    let versioned = root.join("0.3.16");
    std::fs::create_dir_all(&versioned).unwrap();
    let exe = versioned.join("cypher");
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
    // No `current` link yet: not managed by shape.
    assert!(managed_by_layout(&exe).is_none());
    std::os::unix::fs::symlink(&versioned, root.join("current")).unwrap();
    assert_eq!(managed_by_layout(&exe).as_deref(), Some(root.as_path()));
    // A `current` that points elsewhere does not claim this binary.
    let other = root.join("0.3.17");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::remove_file(root.join("current")).unwrap();
    std::os::unix::fs::symlink(&other, root.join("current")).unwrap();
    assert!(managed_by_layout(&exe).is_none());

    let checkout = tmp.path().join("checkout");
    std::fs::create_dir_all(checkout.join("target/debug")).unwrap();
    std::fs::write(checkout.join("Cargo.toml"), "[package]").unwrap();
    assert!(is_source_build(&checkout.join("target/debug/cypher")));
    assert!(!is_source_build(&exe));

    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::fs::write(home.join(".local/bin/cypher"), "hand-copied").unwrap();
    let command = link_command(&home, &root).unwrap();
    assert_eq!(
        std::fs::read_link(&command).unwrap(),
        root.join("current/cypher")
    );
    // Idempotent, and never replaces a directory.
    link_command(&home, &root).unwrap();
    std::fs::remove_file(&command).unwrap();
    std::fs::create_dir(&command).unwrap();
    assert!(link_command(&home, &root).is_err());
}

#[cfg(unix)]
#[test]
fn headless_symlink_swap() {
    let tmp = tempfile::tempdir().unwrap();
    let app_root = tmp.path().join("app");
    for ver in ["0.1.0", "0.1.1"] {
        std::fs::create_dir_all(app_root.join(ver)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let binary = app_root.join(ver).join("cypher");
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    apply_headless(&app_root, "0.1.0").unwrap();
    assert_eq!(
        std::fs::read_link(app_root.join("current")).unwrap(),
        app_root.join("0.1.0")
    );
    // Swap over an existing symlink.
    apply_headless(&app_root, "0.1.1").unwrap();
    assert_eq!(
        std::fs::read_link(app_root.join("current")).unwrap(),
        app_root.join("0.1.1")
    );
    // Unstaged version refuses.
    assert!(apply_headless(&app_root, "0.2.0").is_err());
}
