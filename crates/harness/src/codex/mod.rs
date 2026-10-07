//! Codex catalog: models, effort ladders, and service tiers. The protocol
//! adapter itself is the shared ACP harness ([`crate::AcpHarness::codex`],
//! via the org-maintained `codex-acp` adapter wrapping the codex app-server).

pub(crate) mod catalog;
