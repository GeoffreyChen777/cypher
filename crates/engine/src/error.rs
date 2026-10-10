//! The engine's error type.

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("doc: {0}")]
    Doc(#[from] cypher_doc::DocError),
    #[error("journal: {0}")]
    Journal(#[from] crate::session::journal::JournalError),
    #[error("store: {0}")]
    Store(#[from] cypher_sync::StoreError),
    #[error("{}", harness_message(.0))]
    Harness(#[from] cypher_harness::HarnessError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Harness errors read "harness: …", except a retired harness's message,
/// which is user-facing as-is.
fn harness_message(err: &cypher_harness::HarnessError) -> String {
    match err {
        cypher_harness::HarnessError::Unsupported(message) => message.clone(),
        other => format!("harness: {other}"),
    }
}
