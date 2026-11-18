//! The warm-doc cache: resident-memory budget and LRU eviction of idle docs.

use super::*;

/// Warm-doc LRU: how many unwatched, run-less docs stay fully open. Everything
/// beyond this (and beyond [`cypher_doc::DOC_LRU_BYTE_BUDGET`]) is evicted
/// oldest-access-first — reopening from the SQLite snapshot measured within
/// ~11ms of a warm doc, so the cap trades no perceptible open latency.
const WARM_DOC_CAP: usize = 12;

/// Resident-memory estimate per compressed snapshot byte. Loro snapshots are
/// columnar+compressed; the in-memory doc plus mirror runs well above the blob
/// size. A rough multiplier is enough here — the budget is a safety ceiling,
/// the count cap does the day-to-day work.
pub(super) const RESIDENT_BYTES_PER_SNAPSHOT_BYTE: usize = 6;

/// Floor per open doc (room socket buffers, tasks) regardless of content size.
pub(super) const DOC_RESIDENT_FLOOR_BYTES: usize = 512 * 1024;

/// Docs touched this recently are never evicted. Closes the open→attach race:
/// `open()` returns a handle, and until the caller's `watch_messages` lands
/// the doc is unwatched and unpinned — a concurrent eviction would orphan the
/// watcher on a roomless doc that renders once and never updates again.
const EVICT_MIN_IDLE_MS: i64 = 30_000;

impl DocHost {
    /// LRU eviction: while the warm set exceeds [`WARM_DOC_CAP`] or the
    /// resident estimate exceeds `DOC_LRU_BYTE_BUDGET`, close the
    /// least-recently-touched unpinned docs. Pinned (never evicted):
    /// - watched docs (`messages_tx` has receivers — a UI transcript);
    /// - docs with a live writer (`Arc<SessionDoc>` held outside the handle —
    ///   a run streaming into it);
    /// - host-side docs with pending commands (the executor owes them work).
    ///
    /// Eviction flushes a final snapshot, so reopen loses nothing; missed
    /// remote updates re-arrive through the room join's VV backfill.
    ///
    /// Lock discipline: the `handles` map lock is held only for atomic reads
    /// and the removal itself. Whether a doc is pinned is decided with the
    /// lock released — it reads the doc's ledger and launch records, the
    /// registry and SQLite, and under "Only paired devices" it once went
    /// through `open()`, which re-took this same lock and froze the engine
    /// (every later doc access queued behind the deadlocked thread). The
    /// atomic pins are re-checked under the lock just before removal, so a
    /// doc opened (every `open()` touches it, including the one that queues
    /// a command), watched or given a live writer meanwhile stays.
    pub(super) fn evict_over_budget(&self) {
        let mut by_age: Vec<(i64, Arc<ChatDocHandle>)> = lock(&self.inner.handles)
            .values()
            .map(|h| (h.last_access.load(Ordering::Relaxed), h.clone()))
            .collect();
        by_age.sort_unstable_by_key(|(last_access, _)| *last_access);
        for (last_access, candidate) in by_age {
            if now_ms() - last_access < EVICT_MIN_IDLE_MS {
                // Sorted oldest-first: everything after this is younger.
                return;
            }
            if !self.over_budget() {
                return;
            }
            if candidate.ephemeral.load(Ordering::Acquire) || self.pinned(&candidate) {
                continue;
            }
            let evicted = {
                let mut handles = lock(&self.inner.handles);
                let still_evictable = handles
                    .get(&candidate.chat_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &candidate))
                    && is_idle(&candidate)
                    && !in_use(&candidate);
                if still_evictable {
                    handles.remove(&candidate.chat_id)
                } else {
                    None
                }
            };
            if let Some(handle) = evicted {
                // Final flush outside the map lock; ≤1s of changes could be
                // pending in the snapshot debounce.
                self.save_snapshot(&handle);
                tracing::debug!(chat = %handle.chat_id, "doc evicted (LRU)");
            }
        }
    }
}

/// Not touched for [`EVICT_MIN_IDLE_MS`]. Every `open()` touches, so this is
/// also what keeps a doc someone just opened (to queue a command, attach a
/// watch) from being evicted under them.
fn is_idle(handle: &ChatDocHandle) -> bool {
    now_ms() - handle.last_access.load(Ordering::Relaxed) >= EVICT_MIN_IDLE_MS
}

/// The pins that are plain atomic reads, safe under the map lock: a
/// transcript watcher, or a live writer (the handle itself holds one doc
/// ref; more means a run streaming into it).
fn in_use(handle: &ChatDocHandle) -> bool {
    handle.messages_tx.receiver_count() > 0 || Arc::strong_count(&handle.doc) > 1
}

impl DocHost {
    /// Whether the warm-doc cache resident set is over its count or byte cap.
    fn over_budget(&self) -> bool {
        let handles = lock(&self.inner.handles);
        let estimate = handles
            .values()
            .map(|h| h.resident_estimate())
            .sum::<usize>();
        handles.len() > WARM_DOC_CAP || estimate > cypher_doc::DOC_LRU_BYTE_BUDGET
    }

    /// Whether `handle` must stay open. Called without the `handles` lock
    /// and never takes it: the doc is read through `handle`, never through
    /// `open()`.
    fn pinned(&self, handle: &ChatDocHandle) -> bool {
        if in_use(handle) {
            return true;
        }
        if self.is_host_of(handle) {
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
            match handle.doc.read_commands() {
                Ok(commands) => commands
                    .iter()
                    .any(|c| c.status == SessionCommandStatus::Pending && !is_processed(&c.id)),
                // Unreadable ledger: keep the doc, never evict blind.
                Err(_) => true,
            }
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::TryLockError;
    use std::time::Duration;

    use cypher_doc::LaunchFact;

    use super::*;

    /// "Only paired devices" on, every launch record trusted. With a host
    /// set, each question also checks that the asker doesn't hold the doc
    /// map lock: an eviction that decides under it is one `open()` away
    /// from the freeze this module once had.
    #[derive(Default)]
    struct PairedOnly {
        host: OnceLock<DocHost>,
        asked_under_lock: AtomicBool,
    }

    impl PairedOnly {
        fn probe(&self) {
            let Some(host) = self.host.get() else { return };
            // Another thread's brief hold is fine; a lock held across the
            // question (by the asking thread, or for the whole pass) is not.
            for _ in 0..200 {
                if !matches!(host.inner.handles.try_lock(), Err(TryLockError::WouldBlock)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            self.asked_under_lock.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl CommandTrust for PairedOnly {
        fn sign(&self, _: &[u8]) -> Option<[u8; 64]> {
            Some([7; 64])
        }
        fn is_own_signature(&self, _: &cypher_doc::CommandAuth) -> bool {
            true
        }
        fn is_receive_only(&self, _: &str) -> bool {
            false
        }
        async fn check(&self, _: &str, _: &cypher_doc::CommandAuth) -> Result<(), String> {
            Ok(())
        }
        fn strict(&self) -> bool {
            self.probe();
            true
        }
        fn trusts_record(&self, _: &str, _: &cypher_doc::CommandAuth) -> bool {
            self.probe();
            true
        }
    }

    /// A runtime that never waits on its workers when dropped: a test that
    /// catches a deadlock must fail, not hang on the stuck worker.
    struct Runtime(Option<tokio::runtime::Runtime>);

    impl Drop for Runtime {
        fn drop(&mut self) {
            if let Some(runtime) = self.0.take() {
                runtime.shutdown_background();
            }
        }
    }

    struct Rig {
        host: DocHost,
        trust: Arc<PairedOnly>,
        _dir: tempfile::TempDir,
        runtime: Runtime,
    }

    fn rig() -> Rig {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let host = DocHost::new(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            DocHostConfig {
                device_id: "this-device".into(),
                default_harness: HarnessId::Pi,
                edge: None,
            },
        );
        let trust = Arc::new(PairedOnly::default());
        host.set_command_trust(trust.clone());
        let _ = trust.host.set(host.clone());
        Rig {
            host,
            trust,
            _dir: dir,
            runtime: Runtime(Some(runtime)),
        }
    }

    impl Rig {
        /// Run `f` on its own thread (in the runtime) and fail if it doesn't
        /// return: a deadlock fails the test instead of hanging it.
        fn within_deadline<T: Send + 'static>(
            &self,
            what: &str,
            f: impl FnOnce(DocHost) -> T + Send + 'static,
        ) -> T {
            let runtime = self.runtime.0.as_ref().unwrap().handle().clone();
            let host = self.host.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _entered = runtime.enter();
                let _ = tx.send(f(host));
            });
            rx.recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("{what} never returned: the engine would freeze here"))
        }

        /// Open `chat` and date its last access `idle_s` seconds back.
        fn open_idle(&self, chat: &str, idle_s: i64) -> Arc<ChatDocHandle> {
            let chat = chat.to_string();
            let handle = self.within_deadline("open", move |host| host.open(&chat).unwrap());
            age(&handle, idle_s);
            handle
        }

        fn open_chats(&self) -> Vec<String> {
            let mut chats: Vec<String> = lock(&self.host.inner.handles).keys().cloned().collect();
            chats.sort();
            chats
        }

        fn assert_never_asked_under_lock(&self) {
            assert!(
                !self.trust.asked_under_lock.load(Ordering::SeqCst),
                "trust was asked while the doc map lock was held"
            );
        }
    }

    fn age(handle: &ChatDocHandle, idle_s: i64) {
        handle
            .last_access
            .store(now_ms() - idle_s * 1000, Ordering::Relaxed);
    }

    fn pending_interrupt(id: &str) -> SessionCommandEntry {
        SessionCommandEntry {
            id: id.into(),
            payload: SessionCommandPayload::Interrupt {},
            issued_by: "this-device".into(),
            issued_at: now_ms(),
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
            sent_at: None,
            dismissed: false,
        }
    }

    /// The 2026-11-17 freeze: "Only paired devices" on, 12 warm docs idle,
    /// and a workspace's `start_session` opens a 13th. The open's eviction
    /// pass asked whether an idle doc was pinned while holding the doc map
    /// lock, and the question re-opened the doc through `open()`: the same
    /// lock again, so the engine stopped for good.
    #[test]
    fn opening_past_the_cap_under_only_paired_devices_evicts_and_returns() {
        let rig = rig();
        let _entered = rig.runtime.0.as_ref().unwrap().enter();
        for i in 0..WARM_DOC_CAP {
            rig.open_idle(&format!("chat-{i:02}"), 600 - i as i64);
        }
        rig.within_deadline("opening a doc past the cap", |host| {
            host.open("worker-session").unwrap();
        });
        let open = rig.open_chats();
        assert_eq!(open.len(), WARM_DOC_CAP, "{open:?}");
        assert!(
            !open.contains(&"chat-00".to_string()),
            "the oldest idle doc goes: {open:?}"
        );
        assert!(open.contains(&"worker-session".to_string()));
        rig.assert_never_asked_under_lock();
    }

    /// Under "Only paired devices" the trusted host record in the doc itself
    /// decides whether its pending commands pin it, read from the handle in
    /// hand: deciding must not reopen (and so re-touch) the doc either.
    #[test]
    fn the_trusted_host_record_decides_what_eviction_keeps() {
        let rig = rig();
        let _entered = rig.runtime.0.as_ref().unwrap().enter();
        for (chat, host) in [("ours", "this-device"), ("theirs", "other-device")] {
            let fact = LaunchFact::Host {
                device_id: host.into(),
                cwd: None,
            };
            assert!(rig.host.record_launch(chat, &fact).unwrap());
            rig.host
                .open(chat)
                .unwrap()
                .doc
                .queue_command(&pending_interrupt(&format!("cmd-{chat}")))
                .unwrap();
        }
        let ours = rig.open_idle("ours", 900);
        rig.open_idle("theirs", 800);
        for i in 0..WARM_DOC_CAP - 1 {
            rig.open_idle(&format!("filler-{i:02}"), 600 - i as i64);
        }
        let ours_access = ours.last_access.load(Ordering::Relaxed);
        drop(ours);

        rig.within_deadline("an eviction pass", |host| host.evict_over_budget());

        let open = rig.open_chats();
        assert_eq!(open.len(), WARM_DOC_CAP, "{open:?}");
        assert!(
            open.contains(&"ours".to_string()),
            "a doc hosted here with a pending command stays: {open:?}"
        );
        assert!(
            !open.contains(&"theirs".to_string()),
            "another device's doc is not this host's to keep: {open:?}"
        );
        let ours = lock(&rig.host.inner.handles).get("ours").cloned().unwrap();
        assert_eq!(
            ours.last_access.load(Ordering::Relaxed),
            ours_access,
            "deciding re-opened (and touched) the doc"
        );
        rig.assert_never_asked_under_lock();
    }

    /// The pins re-checked under the lock still hold: a watched doc stays
    /// however old, and the next-oldest idle one goes instead.
    #[test]
    fn a_watched_doc_stays_and_the_next_oldest_goes() {
        let rig = rig();
        let _entered = rig.runtime.0.as_ref().unwrap().enter();
        let watched = rig.open_idle("chat-00", 900);
        let _watch = watched.watch_messages();
        age(&watched, 900);
        drop(watched);
        for i in 1..=WARM_DOC_CAP {
            rig.open_idle(&format!("chat-{i:02}"), 600 - i as i64);
        }

        rig.within_deadline("an eviction pass", |host| host.evict_over_budget());

        let open = rig.open_chats();
        assert_eq!(open.len(), WARM_DOC_CAP, "{open:?}");
        assert!(open.contains(&"chat-00".to_string()), "{open:?}");
        assert!(!open.contains(&"chat-01".to_string()), "{open:?}");
        rig.assert_never_asked_under_lock();
    }
}
