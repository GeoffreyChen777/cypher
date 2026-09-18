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
