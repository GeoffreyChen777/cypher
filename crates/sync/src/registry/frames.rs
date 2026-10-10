//! Registry wire frames (JSON text) — the Rust side of
//! `edge/src/registry-room.ts`; the two change together.

use std::collections::HashMap;

use cypher_doc::{RegistryRow, RowOp};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ClientFrame<'a> {
    Hello {
        cursor: Option<u64>,
        device: &'a str,
    },
    Push {
        batch: &'a str,
        ops: &'a [RowOp],
    },
    Presence {
        at: i64,
        /// The viewport's periodic activity refresh, piggybacked. This frame
        /// already flows every 15s and bills 20:1, so carrying the refresh
        /// here is free, where the identical report over HTTP cost a whole
        /// billable request. Transitions still go over HTTP — only they need
        /// the reply.
        #[serde(skip_serializing_if = "Option::is_none")]
        activity: Option<&'a serde_json::Value>,
    },
    Probe,
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ServerFrame {
    State {
        seq: u64,
        full: bool,
        #[serde(rename = "gcFloor", default)]
        gc_floor: u64,
        rows: Vec<RegistryRow>,
        #[serde(default)]
        presence: HashMap<String, i64>,
    },
    Rows {
        seq: u64,
        rows: Vec<RegistryRow>,
    },
    Ack {
        batch: String,
        seq: u64,
        #[allow(dead_code)]
        applied: u64,
    },
    Presence {
        device: String,
        at: i64,
    },
    #[serde(rename = "probe-ok")]
    ProbeOk {
        #[allow(dead_code)]
        seq: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

// ── transport plumbing (text-frame sibling of chat_client's BinPipe) ───────
