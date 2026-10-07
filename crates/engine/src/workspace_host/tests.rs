use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use cypher_proto::{Device, Session, SessionStatus};
use cypher_sync::DocsStore;

use super::{
    RELAY_PROBE_ALIVE_CAP, RELAY_PROBE_INTERVAL_MS, RelayProbeRetry, WorkspaceHost,
    WorkspaceHostConfig, device_name_on_boot, linked_worktree_root, merge_sessions,
};

fn session(chat_id: &str, device_id: &str, status: SessionStatus) -> Session {
    Session {
        chat_id: chat_id.into(),
        device_id: device_id.into(),
        status,
        started_at: None,
        updated_at: Utc::now(),
        subagents: Vec::new(),
        context_usage: None,
        throughput: None,
    }
}

#[test]
fn merged_sessions_restore_local_durable_rows_after_restart() {
    let durable = vec![
        session("finished-child", "local-device", SessionStatus::Idle),
        session("remote-chat", "remote-device", SessionStatus::Working),
    ];

    let merged = merge_sessions("local-device", &durable, &[]);

    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].chat_id, "finished-child");
    assert_eq!(merged[0].status, SessionStatus::Idle);
    assert_eq!(merged[1].chat_id, "remote-chat");
}

#[test]
fn merged_sessions_prefer_own_live_status_over_durable_status() {
    let durable = vec![session("local-chat", "local-device", SessionStatus::Idle)];
    let live = vec![session(
        "local-chat",
        "local-device",
        SessionStatus::Working,
    )];

    let merged = merge_sessions("local-device", &durable, &live);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].status, SessionStatus::Working);
}

#[test]
fn boot_repairs_the_legacy_unknown_device_sentinel() {
    assert_eq!(
        device_name_on_boot(Some("unknown-device"), "MacBook Pro"),
        "MacBook Pro"
    );
}

#[test]
fn boot_preserves_a_user_selected_device_name() {
    assert_eq!(
        device_name_on_boot(Some("Work laptop"), "MacBook Pro"),
        "Work laptop"
    );
}

#[tokio::test]
async fn local_device_is_fresh_while_the_host_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let host = WorkspaceHost::open(
        Arc::new(DocsStore::open(dir.path()).unwrap()),
        WorkspaceHostConfig {
            device_id: "local-device".into(),
            device_name: "Local".into(),
            platform: "linux".into(),
            org_id: "org".into(),
            user_id: "user".into(),
            edge: None,
            allow_device_rejoin: false,
        },
    )
    .unwrap();
    let mut devices = vec![Device {
        id: "local-device".into(),
        name: "Local".into(),
        platform: "linux".into(),
        last_seen_at: Some(Utc::now() - TimeDelta::minutes(10)),
        created_at: None,
        version: None,
    }];

    host.inner.overlay_presence(&mut devices);

    let age = Utc::now()
        .signed_duration_since(devices[0].last_seen_at.unwrap())
        .num_seconds();
    assert!(age <= 1, "local presence should be fresh, age={age}s");
}

fn open_host(dir: &std::path::Path, device_id: &str, allow_rejoin: bool) -> WorkspaceHost {
    WorkspaceHost::open(
        Arc::new(DocsStore::open(dir).unwrap()),
        WorkspaceHostConfig {
            device_id: device_id.into(),
            device_name: "Local".into(),
            platform: "linux".into(),
            org_id: "org".into(),
            user_id: "user".into(),
            edge: None,
            allow_device_rejoin: allow_rejoin,
        },
    )
    .unwrap()
}

#[cfg(feature = "development")]
#[tokio::test]
async fn seeded_mock_device_is_a_never_seen_peer_and_cannot_replace_self() {
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "local-device", false);

    host.seed_device("mock-box", "Studio Linux box", "linux", None)
        .unwrap();
    assert!(
        host.seed_device("local-device", "Impostor", "linux", None)
            .is_err()
    );

    let devices = host.read_devices().unwrap();
    let mock = devices.iter().find(|d| d.id == "mock-box").unwrap();
    assert_eq!(mock.name, "Studio Linux box");
    assert_eq!(mock.last_seen_at, None);
    let local = devices.iter().find(|d| d.id == "local-device").unwrap();
    assert_eq!(local.name, "Local");
}

fn add_probe_peer(host: &WorkspaceHost) {
    host.mutate(|doc| {
        doc.upsert_device(&Device {
            id: "peer".into(),
            name: "Peer".into(),
            platform: "linux".into(),
            last_seen_at: None,
            created_at: None,
            version: None,
        })
    })
    .unwrap();
}

/// An offline peer is polled forever, by every running engine, at the
/// backoff cap. The unpaced policy cost 120 Durable Object requests per
/// hour per peer; the 5-minute cap cut that to 15, and the 30-minute cap
/// to 7 in the first hour and 2 per hour thereafter — the doubling ramp
/// still answers quickly for a peer that just dropped.
#[tokio::test]
async fn relay_probe_negative_results_pace_an_offline_peer_down_to_the_cap() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "self", false);
    add_probe_peer(&host);
    let start = tokio::time::Instant::now();
    let mut requests = 0;
    let mut second_hour = 0;
    for tick in 0..240 {
        let now = start + Duration::from_secs(tick * 30);
        let due = host.inner.relay_probe_candidates(now);
        assert!(!due.contains(&"self".to_string()));
        for peer in due {
            if tick < 120 {
                requests += 1;
            } else {
                second_hour += 1;
            }
            host.inner.record_relay_probe(&peer, Some(false), now);
        }
    }
    assert_eq!(requests, 7, "first hour, including the doubling ramp");
    assert_eq!(second_hour, 2, "steady state is one probe per cap window");
    let backoff = super::lock(&host.inner.relay_probe_backoff);
    assert_eq!(backoff["peer"].delay, super::RELAY_PROBE_BACKOFF_CAP);
}

#[test]
fn relay_probe_treats_an_unhosted_or_foreign_room_as_an_answer_not_an_error() {
    use reqwest::StatusCode;
    use serde_json::json;
    let answer = super::relay_probe_answer;
    // The body decides only on success.
    let live = json!({ "hostConnected": true });
    let away = json!({ "hostConnected": false });
    assert_eq!(answer(StatusCode::OK, Some(&live)), Some(true));
    assert_eq!(answer(StatusCode::OK, Some(&away)), Some(false));
    assert_eq!(answer(StatusCode::OK, Some(&json!({}))), None);
    assert_eq!(answer(StatusCode::OK, None), None);
    // A room with no owner has never been hosted, and a foreign room is not
    // ours: both are authoritative, so they walk the offline backoff
    // instead of being re-asked every sweep.
    assert_eq!(answer(StatusCode::NOT_FOUND, None), Some(false));
    assert_eq!(answer(StatusCode::FORBIDDEN, None), Some(false));
    // Transient failures say nothing about the peer and must stay
    // inconclusive, or an Edge hiccup would mark every device offline.
    for status in [
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_GATEWAY,
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::UNAUTHORIZED,
    ] {
        assert_eq!(answer(status, None), None, "{status}");
    }
}

#[tokio::test]
async fn relay_probe_errors_do_not_mean_offline_and_alive_answers_back_off() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "self", false);
    add_probe_peer(&host);
    let now = tokio::time::Instant::now();
    host.inner.record_relay_probe("peer", None, now);
    assert!(super::lock(&host.inner.relay_probe_backoff).is_empty());
    assert_eq!(host.inner.relay_probe_candidates(now), vec!["peer"]);
    host.inner.record_relay_probe("peer", Some(false), now);
    assert!(
        host.inner
            .relay_probe_candidates(now + Duration::from_secs(29))
            .is_empty()
    );
    assert_eq!(
        host.inner
            .relay_probe_candidates(now + Duration::from_secs(30)),
        vec!["peer"]
    );
    host.inner
        .record_relay_probe("peer", Some(false), now + Duration::from_secs(30));
    assert!(
        host.inner
            .relay_probe_candidates(now + Duration::from_secs(89))
            .is_empty()
    );
    assert_eq!(
        host.inner
            .relay_probe_candidates(now + Duration::from_secs(90)),
        vec!["peer"]
    );

    // An "alive" answer keeps an entry rather than clearing it. The probe
    // refreshes presence itself, and that self-granted freshness lasts only
    // PRESENCE_FRESH_MS against a 30s sweep — so clearing here is what used
    // to re-poll a healthy, presence-quiet device about once a minute
    // forever (71 Durable Object requests/hour in production).
    assert!(host.inner.record_relay_probe("peer", Some(true), now));
    assert!(!super::lock(&host.inner.relay_probe_backoff).is_empty());
    assert!(host.inner.relay_probe_candidates(now).is_empty());

    // Once that self-granted freshness lapses, the backoff still holds.
    super::lock(&host.inner.presence_seen)
        .insert("peer".into(), crate::now_ms() - super::PRESENCE_FRESH_MS);
    assert!(host.inner.relay_probe_candidates(now).is_empty());
    assert_eq!(
        host.inner
            .relay_probe_candidates(now + Duration::from_secs(30)),
        vec!["peer"]
    );

    // A genuine heartbeat is not the probe's own stamp, so it clears the
    // backoff at once and the device is verified normally again.
    super::lock(&host.inner.presence_seen).insert("peer".into(), crate::now_ms() + 5);
    assert!(host.inner.relay_probe_candidates(now).is_empty());
    assert!(super::lock(&host.inner.relay_probe_backoff).is_empty());
}

#[tokio::test]
async fn relay_probe_fresh_presence_wins_a_late_negative_and_deleted_peers_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "self", false);
    add_probe_peer(&host);
    let now = tokio::time::Instant::now();
    host.inner.record_relay_probe("peer", Some(false), now);
    super::lock(&host.inner.presence_seen).insert("peer".into(), crate::now_ms());
    let mut devices = host.read_devices().unwrap();
    host.inner.overlay_presence(&mut devices);
    assert!(super::lock(&host.inner.relay_probe_backoff).is_empty());
    host.inner.record_relay_probe("peer", Some(false), now);
    assert!(super::lock(&host.inner.relay_probe_backoff).is_empty());

    super::lock(&host.inner.presence_seen).clear();
    host.inner.record_relay_probe("peer", Some(false), now);
    host.delete_device("peer").unwrap();
    assert!(host.inner.relay_probe_candidates(now).is_empty());
    assert!(super::lock(&host.inner.relay_probe_backoff).is_empty());
}

#[tokio::test]
async fn expedited_probe_keeps_the_backoff_ladder() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "self", false);
    add_probe_peer(&host);
    let now = tokio::time::Instant::now();
    for _ in 0..8 {
        host.inner.record_relay_probe("peer", Some(false), now);
    }
    assert!(host.inner.relay_probe_candidates(now).is_empty());

    // A reset makes the peer due at once...
    host.inner.expedite_relay_probes(now);
    assert_eq!(host.inner.relay_probe_candidates(now), vec!["peer"]);
    // ...but another "offline" answer resumes at the cap, not at 30s.
    host.inner.record_relay_probe("peer", Some(false), now);
    assert!(
        host.inner
            .relay_probe_candidates(now + Duration::from_secs(60))
            .is_empty()
    );
    assert_eq!(
        super::lock(&host.inner.relay_probe_backoff)["peer"].delay,
        super::RELAY_PROBE_BACKOFF_CAP
    );
}

#[tokio::test]
async fn foreground_probe_wakes_relay_sweep_and_coalesces_repeated_requests() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "self", false);
    // Opening/focusing a window must not add a probe when no extended
    // offline delay exists yet (in particular during initial join).
    host.probe();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            host.inner.relay_probe_wake.notified()
        )
        .await
        .is_err()
    );
    add_probe_peer(&host);
    host.inner
        .record_relay_probe("peer", Some(false), tokio::time::Instant::now());
    host.probe();
    host.probe();
    tokio::time::timeout(
        Duration::from_secs(1),
        host.inner.relay_probe_wake.notified(),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            host.inner.relay_probe_wake.notified()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn delete_device_refuses_self_and_keeps_peer_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "local-device", false);

    host.mutate(|doc| {
        doc.upsert_device(&Device {
            id: "dev-b".into(),
            name: "vps".into(),
            platform: "linux".into(),
            last_seen_at: None,
            created_at: Some(Utc::now()),
            version: None,
        })
    })
    .unwrap();
    host.create_space("sp-b", "dev-b", "/tmp/b", None, false)
        .unwrap();
    host.create_chat("chat-b", Some("sp-b"), None, None, None)
        .unwrap();

    let err = host.delete_device("local-device").unwrap_err();
    assert!(
        err.to_string().contains("cannot delete this device"),
        "{err}"
    );
    assert_eq!(host.read_devices().unwrap().len(), 2);

    let deleted = host.delete_device("dev-b").unwrap();
    assert!(deleted.existed);
    let devices = host.read_devices().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, "local-device");
    assert_eq!(host.read_spaces().unwrap().len(), 1);
    assert_eq!(host.read_chats().unwrap().len(), 1);
}

#[tokio::test]
async fn unpaired_device_evicts_instead_of_reannouncing() {
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "local-device", false);
    assert!(!*host.watch_evicted().borrow());

    host.mutate(|doc| doc.delete_device("local-device"))
        .unwrap();
    host.reconcile_own_device();
    assert!(*host.watch_evicted().borrow());
    assert!(
        !host
            .read_devices()
            .unwrap()
            .iter()
            .any(|d| d.id == "local-device")
    );
}

#[tokio::test]
async fn fresh_sign_in_may_revive_a_tombstoned_device() {
    let dir = tempfile::tempdir().unwrap();
    let host = open_host(dir.path(), "local-device", true);
    host.mutate(|doc| doc.delete_device("local-device"))
        .unwrap();
    host.reconcile_own_device();
    assert!(!*host.watch_evicted().borrow());
    assert!(
        host.read_devices()
            .unwrap()
            .iter()
            .any(|d| d.id == "local-device")
    );
}

#[test]
fn linked_worktree_resolves_to_the_checkout_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let wt = dir.path().join("clever-ember");
    std::fs::create_dir_all(root.join(".git").join("worktrees").join("clever-ember")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!(
            "gitdir: {}\n",
            root.join(".git/worktrees/clever-ember").display()
        ),
    )
    .unwrap();
    assert_eq!(
        linked_worktree_root(&wt).as_deref(),
        Some(root.to_str().unwrap())
    );
}

#[test]
fn primary_checkouts_and_plain_folders_resolve_to_none() {
    let dir = tempfile::tempdir().unwrap();
    // Primary checkout: `.git` is a directory.
    let primary = dir.path().join("primary");
    std::fs::create_dir_all(primary.join(".git")).unwrap();
    assert_eq!(linked_worktree_root(&primary), None);
    // Not a repo at all.
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(linked_worktree_root(&plain), None);
    // A `.git` file pointing somewhere that is not `<root>/.git/worktrees/<name>`.
    let odd = dir.path().join("odd");
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(odd.join(".git"), "gitdir: /somewhere/else\n").unwrap();
    assert_eq!(linked_worktree_root(&odd), None);
}

/// A device that answers "alive" while its presence beat stays silent used
/// to be re-probed every sweep, forever: a successful probe cleared the
/// backoff and granted only PRESENCE_FRESH_MS (45s) of freshness against a
/// 30s sweep. Measured at 71 Durable Object requests/hour in production.
#[test]
fn a_live_but_presence_quiet_device_stops_being_polled() {
    let now = tokio::time::Instant::now();

    // First probe: nothing known yet, so it starts at the sweep interval.
    let first = RelayProbeRetry::alive(None, now, 1_000);
    assert_eq!(
        first.delay,
        std::time::Duration::from_millis(RELAY_PROBE_INTERVAL_MS)
    );
    assert_eq!(first.verified_at, Some(1_000));

    // Each further "alive" answer doubles the wait, up to the cap.
    let second = RelayProbeRetry::alive(Some(&first), now, 2_000);
    assert_eq!(second.delay, first.delay * 2);
    let mut retry = second;
    for stamp in 0..10 {
        retry = RelayProbeRetry::alive(Some(&retry), now, stamp);
    }
    assert_eq!(retry.delay, RELAY_PROBE_ALIVE_CAP);
    assert!(retry.retry_at > now);
}

/// The backoff only holds because the probe's own stamp is not mistaken for
/// a heartbeat. A genuine beat still clears it immediately.
#[test]
fn only_a_genuine_heartbeat_clears_the_alive_backoff() {
    let now = tokio::time::Instant::now();
    let probed = RelayProbeRetry::alive(None, now, 5_000);

    // Freshness the probe granted itself: recognised, backoff survives.
    assert_eq!(probed.verified_at, Some(5_000));

    // A real presence frame carries a different, newer timestamp, so the
    // candidate filter's `verified_at == seen` test fails and it clears.
    assert_ne!(probed.verified_at, Some(6_000));
}

/// An offline answer must not inherit an alive entry's delay, or a device
/// that just went away would start its offline backoff already near the cap
/// and be noticed far too late.
#[test]
fn going_offline_restarts_the_backoff_from_the_sweep_interval() {
    let now = tokio::time::Instant::now();
    let mut alive = RelayProbeRetry::alive(None, now, 1);
    for _ in 0..6 {
        alive = RelayProbeRetry::alive(Some(&alive), now, 2);
    }
    assert_eq!(alive.delay, RELAY_PROBE_ALIVE_CAP);

    let offline = RelayProbeRetry::offline(Some(&alive), now);
    assert_eq!(
        offline.delay,
        std::time::Duration::from_millis(RELAY_PROBE_INTERVAL_MS)
    );
    assert_eq!(offline.verified_at, None);

    // Repeated offline answers still double as before.
    let again = RelayProbeRetry::offline(Some(&offline), now);
    assert_eq!(again.delay, offline.delay * 2);
}

/// Presence beats republish everything every 15s per device. Waking every
/// watch stream for an identical snapshot cost a relay frame to each
/// subscribed viewport — 1,050 inbound websocket messages/hour on one
/// DeviceRoom in production, all redundant.
#[test]
fn republishing_an_identical_snapshot_wakes_nobody() {
    let (tx, mut rx) = tokio::sync::watch::channel(Vec::<String>::new());

    super::publish_if_changed(&tx, vec!["a".to_string()]);
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();

    // The same snapshot again: stored, but no wake-up.
    super::publish_if_changed(&tx, vec!["a".to_string()]);
    super::publish_if_changed(&tx, vec!["a".to_string()]);
    assert!(!rx.has_changed().unwrap());

    // A real change still propagates immediately.
    super::publish_if_changed(&tx, vec!["a".to_string(), "b".to_string()]);
    assert!(rx.has_changed().unwrap());
    assert_eq!(
        *rx.borrow_and_update(),
        vec!["a".to_string(), "b".to_string()]
    );
}

/// The value must be written through even when nobody is listening yet, or
/// a stream subscribed later would start from a stale snapshot.
#[test]
fn the_latest_snapshot_is_stored_for_a_later_subscriber() {
    let (tx, rx) = tokio::sync::watch::channel(Vec::<String>::new());
    drop(rx);
    super::publish_if_changed(&tx, vec!["fresh".to_string()]);
    assert_eq!(*tx.subscribe().borrow(), vec!["fresh".to_string()]);
}
