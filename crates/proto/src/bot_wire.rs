//! The bot network's wire shapes (docs/design/agent-bots.md §11): the
//! frames a `BotRoom` hands its host engine on `GET /bots/:id/inbox`, and
//! the bodies of the other `/bots/:id/*` routes.
//!
//! Mirrored by `apps/edge/src/bots/bot-frames.ts`; both run
//! `protocol/vectors/bot-frames-v1.json` (protocol/README.md). Field names
//! are camelCase; exchange states on the wire are snake_case, as the design
//! writes them. Times are epoch milliseconds.

use serde::{Deserialize, Serialize};

use crate::ExchangeState;

/// Longest question, in UTF-8 bytes.
pub const QUESTION_MAX_BYTES: usize = 8 * 1024;
/// Longest answer, in UTF-8 bytes.
pub const WIRE_ANSWER_MAX_BYTES: usize = 64 * 1024;
/// Per connection, on both sides.
pub const ASKS_PER_HOUR: u32 = 30;
pub const ASKS_PER_DAY: u32 = 200;
pub const ASKS_IN_FLIGHT: u32 = 3;
/// An invite's default lifetime and uses.
pub const INVITE_TTL_MS: i64 = 7 * 24 * 3_600_000;
pub const INVITE_MAX_USES: u32 = 1;
/// An exchange not settled this long after it was sent expires.
pub const EXCHANGE_TTL_MS: i64 = 7 * 24 * 3_600_000;

/// One entry for a bot's host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum BotFrame {
    /// A connected bot asks this bot a question.
    #[serde(rename_all = "camelCase")]
    Ask {
        exchange_id: String,
        connection_id: String,
        text: String,
        sent_at: i64,
    },
    /// The outcome of a question this bot asked.
    #[serde(rename_all = "camelCase")]
    Answer {
        exchange_id: String,
        connection_id: String,
        status: AnswerStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        answered_at: i64,
    },
    /// Progress of a question this bot asked.
    #[serde(rename_all = "camelCase")]
    State {
        exchange_id: String,
        state: WireState,
    },
    /// A connection was made (by a peer redeeming an invite, or by this bot
    /// redeeming one) or revoked (by either side).
    #[serde(rename_all = "camelCase")]
    Link {
        connection_id: String,
        op: LinkOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer: Option<WirePeer>,
        /// The invite a peer redeemed, on the inviting side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invite_id: Option<String>,
    },
    /// A frame from a newer Edge: acknowledged and skipped.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireState {
    Queued,
    Delivered,
    Working,
    AwaitingReview,
    Answered,
    Declined,
    Expired,
}

impl WireState {
    pub fn is_final(self) -> bool {
        matches!(self, Self::Answered | Self::Declined | Self::Expired)
    }
}

impl From<WireState> for ExchangeState {
    fn from(state: WireState) -> Self {
        match state {
            WireState::Queued => Self::Queued,
            WireState::Delivered => Self::Delivered,
            WireState::Working => Self::Working,
            WireState::AwaitingReview => Self::AwaitingReview,
            WireState::Answered => Self::Answered,
            WireState::Declined => Self::Declined,
            WireState::Expired => Self::Expired,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnswerStatus {
    Answered,
    Declined,
    Expired,
}

impl From<AnswerStatus> for WireState {
    fn from(status: AnswerStatus) -> Self {
        match status {
            AnswerStatus::Answered => Self::Answered,
            AnswerStatus::Declined => Self::Declined,
            AnswerStatus::Expired => Self::Expired,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkOp {
    Added,
    Revoked,
}

/// A bot as its owner published it, addressed by its room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WirePeer {
    pub bot_room_id: String,
    pub name: String,
    #[serde(default)]
    pub owner_name: String,
}

impl From<WirePeer> for crate::PeerBot {
    fn from(peer: WirePeer) -> Self {
        Self {
            room_id: peer.bot_room_id,
            name: peer.name,
            owner_name: peer.owner_name,
        }
    }
}

// ── route bodies ────────────────────────────────────────────────────────────

/// `POST /bots/:id/publish`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRequest {
    pub name: String,
    pub owner_name: String,
    pub host_device_id: String,
    #[serde(default)]
    pub paused: bool,
}

/// `POST /bots/:id/invites`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteCreated {
    pub invite_id: String,
    /// `<botRoomId>.<secret>`; the room keeps only the secret's hash.
    pub code: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub max_uses: u32,
}

/// `POST /bots/:id/connect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connected {
    pub connection_id: String,
    pub peer: WirePeer,
    pub created_at: i64,
}

/// `POST /bots/:id/ask`. The asking engine mints the exchange id, so a
/// retried ask is one exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskRequest {
    pub exchange_id: String,
    pub connection_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskSent {
    pub exchange_id: String,
    pub sent_at: i64,
}

/// `POST /bots/:id/answer`: `answered` (with text) or `declined`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerRequest {
    pub exchange_id: String,
    pub status: AnswerStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// `POST /bots/:id/state`: `working` or `awaiting_review`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateRequest {
    pub exchange_id: String,
    pub state: WireState,
}

/// `GET /bots/:id/inbox`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxPage {
    #[serde(default)]
    pub entries: Vec<BotFrame>,
}

/// `POST /bots/:id/ack`: what the host has written down. `asks` names
/// `ask` entries, `states` the `state` and `answer` entries of this bot's
/// own questions (at the state they reported), `links` the `link` entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<StateAck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

impl AckRequest {
    pub fn is_empty(&self) -> bool {
        self.asks.is_empty() && self.states.is_empty() && self.links.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateAck {
    pub exchange_id: String,
    pub state: WireState,
}

/// Every `/bots/*` error: `{error, message?}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireError {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Split an invite code into the inviting bot's room id and the secret.
pub fn split_invite_code(code: &str) -> Option<(&str, &str)> {
    let (room, secret) = code.trim().split_once('.')?;
    let id_ok = |s: &str, max: usize| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    (id_ok(room, 128) && id_ok(secret, 128)).then_some((room, secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Vectors {
        frames: Vec<FrameCase>,
        invalid: Vec<InvalidCase>,
        codes: Vec<CodeCase>,
    }

    #[derive(Deserialize)]
    struct FrameCase {
        name: String,
        frame: serde_json::Value,
    }

    #[derive(Deserialize)]
    struct InvalidCase {
        name: String,
        frame: serde_json::Value,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CodeCase {
        code: String,
        room: Option<String>,
        secret: Option<String>,
    }

    fn vectors() -> Vectors {
        serde_json::from_str(include_str!("../../../protocol/vectors/bot-frames-v1.json"))
            .expect("vectors parse")
    }

    #[test]
    fn shared_vectors_round_trip() {
        for case in vectors().frames {
            let frame: BotFrame = serde_json::from_value(case.frame.clone())
                .unwrap_or_else(|err| panic!("{}: {err}", case.name));
            assert_ne!(frame, BotFrame::Unknown, "{}", case.name);
            assert_eq!(
                serde_json::to_value(&frame).unwrap(),
                case.frame,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn shared_vectors_reject_malformed_frames() {
        for case in vectors().invalid {
            assert!(
                serde_json::from_value::<BotFrame>(case.frame).is_err(),
                "{} must not decode",
                case.name
            );
        }
    }

    #[test]
    fn shared_vectors_split_invite_codes() {
        for case in vectors().codes {
            let split = split_invite_code(&case.code);
            assert_eq!(
                split.map(|(r, s)| (r.to_string(), s.to_string())),
                case.room.zip(case.secret),
                "{}",
                case.code
            );
        }
    }

    #[test]
    fn unknown_frames_decode_to_unknown() {
        let frame: BotFrame =
            serde_json::from_value(serde_json::json!({"t": "group", "x": 1})).unwrap();
        assert_eq!(frame, BotFrame::Unknown);
    }

    #[test]
    fn wire_states_map_onto_rows() {
        assert_eq!(
            ExchangeState::from(WireState::AwaitingReview),
            ExchangeState::AwaitingReview
        );
        assert!(WireState::from(AnswerStatus::Expired).is_final());
        assert!(!WireState::Working.is_final());
        assert_eq!(
            serde_json::to_value(WireState::AwaitingReview).unwrap(),
            "awaiting_review"
        );
    }
}
