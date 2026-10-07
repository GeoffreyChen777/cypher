//! Fixtures shared by the registry integration-test binaries. Each binary
//! uses a subset, hence the blanket `dead_code` allowance.
#![allow(dead_code)]

use std::time::Duration;

use chrono::{DateTime, Utc};
use cypher_proto::{Chat, Device};

pub fn ts(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::UNIX_EPOCH)
}

pub fn device(id: &str) -> Device {
    Device {
        id: id.into(),
        name: format!("{id}-name"),
        platform: "linux".into(),
        last_seen_at: Some(ts(1_000)),
        created_at: Some(ts(500)),
        version: Some("0.1.0".into()),
    }
}

pub fn chat(id: &str, device_id: &str) -> Chat {
    Chat {
        pinned: false,
        id: id.into(),
        device_id: device_id.into(),
        title: Some("chat".into()),
        archived: false,
        cwd: Some("/tmp".into()),
        branch: None,
        checkout_id: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: ts(2_000),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        child: None,
    }
}

/// Poll `condition` until it holds, panicking after `timeout`.
pub async fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(timeout, async {
        loop {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition not reached in time");
}
