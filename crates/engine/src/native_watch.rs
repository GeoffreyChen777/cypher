//! Native watcher calls can wait synchronously on the OS. Keep creation,
//! registration and destruction off async executors AND Tokio's blocking pool:
//! runtime shutdown joins that pool and would hang on a stuck registration.

/// The worker owns the native handle. Dropping this sender disposes of a handle
/// even if its creation finishes late; neither close nor runtime teardown waits
/// for the native call. The OS call itself is not forcibly cancellable.
pub(crate) struct BackgroundWatch {
    _stop: std::sync::mpsc::Sender<()>,
}

impl BackgroundWatch {
    pub(crate) fn start<T: Send + 'static>(
        create: impl FnOnce() -> Option<T> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("cypher-native-watch".into())
            .spawn(move || {
                if let Some(watcher) = create() {
                    let _ = stopped.recv();
                    drop(watcher);
                }
            })?;
        Ok(Self { _stop: stop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn runtime_teardown_does_not_join_a_pending_native_registration() {
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (runtime_gone_tx, runtime_gone_rx) = std::sync::mpsc::channel();
        let (native_done_tx, native_done_rx) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let watch = BackgroundWatch::start(move || {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(5));
                    let _ = native_done_tx.send(());
                    None::<()>
                })
                .unwrap();
                entered_rx.await.unwrap();
                drop(watch);
            });
            drop(runtime);
            runtime_gone_tx.send(()).unwrap();
        });
        // Release the simulated OS call even if the assertion fails, so the
        // test itself never leaves a permanently blocked worker.
        let stopped = runtime_gone_rx.recv_timeout(Duration::from_secs(2));
        let _ = release_tx.send(());
        native_done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.join().unwrap();
        assert!(
            stopped.is_ok(),
            "runtime shutdown waited for native registration"
        );
    }
}
