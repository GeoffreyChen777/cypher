//! Fresh workspace v3. Authoritative metadata and its outbox are local SQLite,
//! not a serialized registry1 document. Control/presence never enter this DB.
pub mod client;
#[cfg(test)]
mod client_tests;
pub mod journal;
#[cfg(test)]
mod tests;
pub mod wire;
