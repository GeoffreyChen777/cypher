//! Optional pre-commit coverage hook. Default-off and entirely in-memory.
//! A coverage marker is durable; preview text itself is never imported here.
use crate::MessagePart;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCoverage {
    pub run_id: String,
    pub segment_id: String,
    pub epoch: String,
    pub revision: u64,
    pub complete: bool,
}

pub type PreviewCommitHook =
    Arc<dyn Fn(&str, &[MessagePart], bool) -> Option<PreviewCoverage> + Send + Sync>;
