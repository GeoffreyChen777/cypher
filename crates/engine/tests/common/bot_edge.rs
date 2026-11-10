//! An in-process fake of the Edge's bot network (apps/edge/src/bots/
//! bot-room.ts): rooms with owners, invites, connections, inboxes and
//! outboxes, and nudges to the host engine. Each engine gets a
//! [`FakeEdge`] acting as one user; the [`FakeHub`] is the shared Edge.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use cypher_engine::bots::{BotEdge, Bots, NetError};
use cypher_proto::{
    AckRequest, AnswerRequest, AnswerStatus, AskRequest, AskSent, BotFrame, ConnectRequest,
    Connected, InboxPage, InviteCreated, InviteRequest, LinkOp, PublishRequest, StateRequest,
    WirePeer, WireState,
};

#[derive(Default)]
pub struct FakeHub {
    state: Mutex<Hub>,
}

#[derive(Default)]
struct Hub {
    rooms: HashMap<String, Room>,
    /// device id → (owner, the engine's bot service) for nudges.
    devices: HashMap<String, (String, Bots)>,
    next: u64,
    /// Every call, as `user method room`.
    calls: Vec<String>,
}

struct Room {
    owner: String,
    profile: PublishRequest,
    invites: Vec<Invite>,
    connections: Vec<Conn>,
    inbox: Vec<Incoming>,
    outbox: Vec<Outgoing>,
}

struct Invite {
    id: String,
    secret: String,
    expires_at: i64,
    max_uses: u32,
    uses: u32,
    revoked: bool,
}

struct Conn {
    id: String,
    peer_room: String,
    peer: WirePeer,
    revoked: bool,
    invite_id: Option<String>,
    link_pending: Option<LinkOp>,
}

struct Incoming {
    exchange_id: String,
    connection_id: String,
    text: String,
    state: WireState,
    sent_at: i64,
    acked: bool,
}

struct Outgoing {
    exchange_id: String,
    connection_id: String,
    state: WireState,
    text: Option<String>,
    answered_at: Option<i64>,
    acked_state: Option<WireState>,
}

fn refuse(status: u16, code: &str) -> NetError {
    NetError::new(Some(status), code, format!("fake edge: {code}"))
}

impl FakeHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// An Edge client acting as `user`.
    pub fn client(self: &Arc<Self>, user: &str) -> Arc<FakeEdge> {
        Arc::new(FakeEdge {
            hub: self.clone(),
            user: user.to_string(),
        })
    }

    /// Deliver `bots` nudges for rooms hosted on `device` (owned by `user`)
    /// to `bots`, as the device's DeviceRoom would.
    pub fn register_device(&self, device: &str, user: &str, bots: Bots) {
        self.state
            .lock()
            .unwrap()
            .devices
            .insert(device.to_string(), (user.to_string(), bots));
    }

    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    /// The published profile of `room`.
    pub fn profile(&self, room: &str) -> Option<PublishRequest> {
        self.state
            .lock()
            .unwrap()
            .rooms
            .get(room)
            .map(|r| r.profile.clone())
    }

    /// Questions the room holds for its host that the host has not acked.
    pub fn pending_asks(&self, room: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .rooms
            .get(room)
            .map_or(0, |r| r.inbox.iter().filter(|i| !i.acked).count())
    }

    fn nudge(&self, nudges: Vec<(String, Bots)>) {
        for (room, bots) in nudges {
            bots.bots_nudged(room);
        }
    }
}

impl Hub {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{}", self.next)
    }

    fn owned(&mut self, user: &str, room: &str) -> Result<&mut Room, NetError> {
        match self.rooms.get_mut(room) {
            None => Err(refuse(404, "not_published")),
            Some(r) if r.owner != user => Err(refuse(403, "forbidden")),
            Some(r) => Ok(r),
        }
    }

    /// The engine to nudge for `room`, when its host device is its owner's.
    fn nudge_for(&self, room: &str) -> Vec<(String, Bots)> {
        let Some(r) = self.rooms.get(room) else {
            return Vec::new();
        };
        match self.devices.get(&r.profile.host_device_id) {
            Some((owner, bots)) if *owner == r.owner => vec![(room.to_string(), bots.clone())],
            _ => Vec::new(),
        }
    }

    fn self_peer(&self, room: &str) -> WirePeer {
        let r = &self.rooms[room];
        WirePeer {
            bot_room_id: room.to_string(),
            name: r.profile.name.clone(),
            owner_name: r.profile.owner_name.clone(),
            owner_avatar_url: r.profile.owner_avatar_url.clone(),
        }
    }

    fn live_conn<'a>(&'a mut self, room: &str, connection_id: &str) -> Option<&'a mut Conn> {
        self.rooms
            .get_mut(room)?
            .connections
            .iter_mut()
            .find(|c| c.id == connection_id && !c.revoked)
    }
}

pub struct FakeEdge {
    hub: Arc<FakeHub>,
    user: String,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[async_trait]
impl BotEdge for FakeEdge {
    async fn publish(&self, room: &str, request: &PublishRequest) -> Result<(), NetError> {
        let mut nudges = Vec::new();
        {
            let mut hub = self.hub.state.lock().unwrap();
            hub.calls.push(format!("{} publish {room}", self.user));
            match hub.rooms.get_mut(room) {
                Some(r) if r.owner != self.user => return Err(refuse(403, "forbidden")),
                Some(r) => {
                    let changed = r.profile.name != request.name
                        || r.profile.owner_name != request.owner_name
                        || r.profile.owner_avatar_url != request.owner_avatar_url;
                    r.profile = request.clone();
                    // Like the Edge: a changed profile reaches every room this
                    // one is connected to, as another `added` link.
                    if changed {
                        let me = hub.self_peer(room);
                        let links: Vec<(String, String)> = hub.rooms[room]
                            .connections
                            .iter()
                            .filter(|c| !c.revoked)
                            .map(|c| (c.peer_room.clone(), c.id.clone()))
                            .collect();
                        for (peer_room, connection_id) in links {
                            if let Some(conn) = hub.live_conn(&peer_room, &connection_id) {
                                conn.peer = me.clone();
                                conn.link_pending = Some(LinkOp::Added);
                            }
                            nudges.extend(hub.nudge_for(&peer_room));
                        }
                    }
                }
                None => {
                    hub.rooms.insert(
                        room.to_string(),
                        Room {
                            owner: self.user.clone(),
                            profile: request.clone(),
                            invites: Vec::new(),
                            connections: Vec::new(),
                            inbox: Vec::new(),
                            outbox: Vec::new(),
                        },
                    );
                }
            }
        }
        self.hub.nudge(nudges);
        Ok(())
    }

    async fn inbox(&self, room: &str) -> Result<InboxPage, NetError> {
        let mut hub = self.hub.state.lock().unwrap();
        hub.calls.push(format!("{} inbox {room}", self.user));
        let r = hub.owned(&self.user, room)?;
        let mut entries = Vec::new();
        for c in &r.connections {
            match c.link_pending {
                Some(LinkOp::Added) => entries.push(BotFrame::Link {
                    connection_id: c.id.clone(),
                    op: LinkOp::Added,
                    peer: Some(c.peer.clone()),
                    invite_id: c.invite_id.clone(),
                }),
                Some(LinkOp::Revoked) => entries.push(BotFrame::Link {
                    connection_id: c.id.clone(),
                    op: LinkOp::Revoked,
                    peer: None,
                    invite_id: None,
                }),
                None => {}
            }
        }
        for i in r
            .inbox
            .iter()
            .filter(|i| !i.acked && i.state == WireState::Queued)
        {
            entries.push(BotFrame::Ask {
                exchange_id: i.exchange_id.clone(),
                connection_id: i.connection_id.clone(),
                text: i.text.clone(),
                sent_at: i.sent_at,
            });
        }
        for o in r.outbox.iter().filter(|o| o.acked_state != Some(o.state)) {
            entries.push(if o.state.is_final() {
                BotFrame::Answer {
                    exchange_id: o.exchange_id.clone(),
                    connection_id: o.connection_id.clone(),
                    status: match o.state {
                        WireState::Answered => AnswerStatus::Answered,
                        WireState::Declined => AnswerStatus::Declined,
                        _ => AnswerStatus::Expired,
                    },
                    text: o.text.clone(),
                    answered_at: o.answered_at.unwrap_or_else(now),
                }
            } else {
                BotFrame::State {
                    exchange_id: o.exchange_id.clone(),
                    state: o.state,
                }
            });
        }
        Ok(InboxPage { entries })
    }

    async fn create_invite(
        &self,
        room: &str,
        request: &InviteRequest,
    ) -> Result<InviteCreated, NetError> {
        let mut hub = self.hub.state.lock().unwrap();
        hub.calls.push(format!("{} invite {room}", self.user));
        let id = hub.id("inv");
        let secret = hub.id("secret");
        let r = hub.owned(&self.user, room)?;
        let created_at = now();
        let expires_at = created_at + request.ttl_ms.unwrap_or(cypher_proto::INVITE_TTL_MS);
        let max_uses = request.max_uses.unwrap_or(cypher_proto::INVITE_MAX_USES);
        r.invites.push(Invite {
            id: id.clone(),
            secret: secret.clone(),
            expires_at,
            max_uses,
            uses: 0,
            revoked: false,
        });
        Ok(InviteCreated {
            invite_id: id,
            code: format!("{room}.{secret}"),
            created_at,
            expires_at,
            max_uses,
        })
    }

    async fn revoke_invite(&self, room: &str, invite_id: &str) -> Result<(), NetError> {
        let mut hub = self.hub.state.lock().unwrap();
        hub.calls
            .push(format!("{} revoke-invite {room}", self.user));
        let r = hub.owned(&self.user, room)?;
        let invite = r
            .invites
            .iter_mut()
            .find(|i| i.id == invite_id)
            .ok_or_else(|| refuse(404, "no_invite"))?;
        invite.revoked = true;
        Ok(())
    }

    async fn connect(&self, room: &str, request: &ConnectRequest) -> Result<Connected, NetError> {
        let nudges;
        let connected;
        {
            let mut hub = self.hub.state.lock().unwrap();
            hub.calls.push(format!("{} connect {room}", self.user));
            hub.owned(&self.user, room)?;
            let (peer_room, secret) = cypher_proto::split_invite_code(&request.code)
                .ok_or_else(|| refuse(400, "bad_code"))?;
            let (peer_room, secret) = (peer_room.to_string(), secret.to_string());
            let me = hub.self_peer(room);
            let connection_id = hub.id("cn");
            let peer = {
                let user = self.user.clone();
                let target = hub
                    .rooms
                    .get_mut(&peer_room)
                    .ok_or_else(|| refuse(404, "invite_invalid"))?;
                if target.owner == user {
                    return Err(refuse(403, "same_owner"));
                }
                if target
                    .connections
                    .iter()
                    .any(|c| c.peer_room == room && !c.revoked)
                {
                    return Err(refuse(409, "already_connected"));
                }
                let invite = target
                    .invites
                    .iter_mut()
                    .find(|i| i.secret == secret)
                    .filter(|i| !i.revoked && i.uses < i.max_uses && i.expires_at > now())
                    .ok_or_else(|| refuse(404, "invite_invalid"))?;
                invite.uses += 1;
                let invite_id = invite.id.clone();
                target.connections.push(Conn {
                    id: connection_id.clone(),
                    peer_room: room.to_string(),
                    peer: me,
                    revoked: false,
                    invite_id: Some(invite_id),
                    link_pending: Some(LinkOp::Added),
                });
                WirePeer {
                    bot_room_id: peer_room.clone(),
                    name: target.profile.name.clone(),
                    owner_name: target.profile.owner_name.clone(),
                    owner_avatar_url: target.profile.owner_avatar_url.clone(),
                }
            };
            let created_at = now();
            hub.rooms
                .get_mut(room)
                .expect("own room")
                .connections
                .push(Conn {
                    id: connection_id.clone(),
                    peer_room: peer_room.clone(),
                    peer: peer.clone(),
                    revoked: false,
                    invite_id: None,
                    link_pending: Some(LinkOp::Added),
                });
            nudges = hub.nudge_for(&peer_room);
            connected = Connected {
                connection_id,
                peer,
                created_at,
            };
        }
        self.hub.nudge(nudges);
        Ok(connected)
    }

    async fn revoke_connection(&self, room: &str, connection_id: &str) -> Result<(), NetError> {
        let nudges;
        {
            let mut hub = self.hub.state.lock().unwrap();
            hub.calls.push(format!("{} revoke {room}", self.user));
            let r = hub.owned(&self.user, room)?;
            let conn = r
                .connections
                .iter_mut()
                .find(|c| c.id == connection_id)
                .ok_or_else(|| refuse(404, "no_connection"))?;
            if conn.revoked {
                return Ok(());
            }
            conn.revoked = true;
            conn.link_pending = None;
            let peer_room = conn.peer_room.clone();
            expire(r, connection_id);
            if let Some(peer) = hub.rooms.get_mut(&peer_room)
                && let Some(conn) = peer
                    .connections
                    .iter_mut()
                    .find(|c| c.id == connection_id && c.peer_room == room)
            {
                conn.revoked = true;
                conn.link_pending = Some(LinkOp::Revoked);
                expire(peer, connection_id);
            }
            nudges = hub.nudge_for(&peer_room);
        }
        self.hub.nudge(nudges);
        Ok(())
    }

    async fn ask(&self, room: &str, request: &AskRequest) -> Result<AskSent, NetError> {
        let nudges;
        let sent_at = now();
        {
            let mut hub = self.hub.state.lock().unwrap();
            hub.calls.push(format!("{} ask {room}", self.user));
            hub.owned(&self.user, room)?;
            if request.text.len() > cypher_proto::QUESTION_MAX_BYTES {
                return Err(refuse(413, "too_large"));
            }
            let peer_room = hub
                .live_conn(room, &request.connection_id)
                .ok_or_else(|| refuse(404, "no_connection"))?
                .peer_room
                .clone();
            let in_flight = hub.rooms[room]
                .outbox
                .iter()
                .filter(|o| o.connection_id == request.connection_id && !o.state.is_final())
                .count();
            if in_flight >= cypher_proto::ASKS_IN_FLIGHT as usize {
                return Err(refuse(429, "too_many_in_flight"));
            }
            hub.live_conn(&peer_room, &request.connection_id)
                .ok_or_else(|| refuse(404, "no_connection"))?;
            let peer = hub.rooms.get_mut(&peer_room).expect("peer room");
            peer.inbox.push(Incoming {
                exchange_id: request.exchange_id.clone(),
                connection_id: request.connection_id.clone(),
                text: request.text.clone(),
                state: WireState::Queued,
                sent_at,
                acked: false,
            });
            hub.rooms
                .get_mut(room)
                .expect("own room")
                .outbox
                .push(Outgoing {
                    exchange_id: request.exchange_id.clone(),
                    connection_id: request.connection_id.clone(),
                    state: WireState::Queued,
                    text: None,
                    answered_at: None,
                    acked_state: None,
                });
            nudges = hub.nudge_for(&peer_room);
        }
        self.hub.nudge(nudges);
        Ok(AskSent {
            exchange_id: request.exchange_id.clone(),
            sent_at,
        })
    }

    async fn answer(&self, room: &str, request: &AnswerRequest) -> Result<(), NetError> {
        let nudges;
        {
            let mut hub = self.hub.state.lock().unwrap();
            hub.calls
                .push(format!("{} answer {room} {:?}", self.user, request.status));
            let r = hub.owned(&self.user, room)?;
            let incoming = r
                .inbox
                .iter_mut()
                .find(|i| i.exchange_id == request.exchange_id)
                .ok_or_else(|| refuse(404, "no_exchange"))?;
            if incoming.state.is_final() {
                return Err(refuse(409, "settled"));
            }
            incoming.state = request.status.into();
            let connection_id = incoming.connection_id.clone();
            let Some(peer_room) = hub
                .live_conn(room, &connection_id)
                .map(|c| c.peer_room.clone())
            else {
                return Ok(());
            };
            if let Some(peer) = hub.rooms.get_mut(&peer_room)
                && let Some(out) = peer
                    .outbox
                    .iter_mut()
                    .find(|o| o.exchange_id == request.exchange_id && !o.state.is_final())
            {
                out.state = request.status.into();
                out.text = request.text.clone();
                out.answered_at = Some(now());
            }
            nudges = hub.nudge_for(&peer_room);
        }
        self.hub.nudge(nudges);
        Ok(())
    }

    async fn state(&self, room: &str, request: &StateRequest) -> Result<(), NetError> {
        let mut hub = self.hub.state.lock().unwrap();
        hub.calls
            .push(format!("{} state {room} {:?}", self.user, request.state));
        let r = hub.owned(&self.user, room)?;
        let incoming = r
            .inbox
            .iter_mut()
            .find(|i| i.exchange_id == request.exchange_id)
            .ok_or_else(|| refuse(404, "no_exchange"))?;
        if incoming.state.is_final() {
            return Err(refuse(409, "settled"));
        }
        incoming.state = request.state;
        let connection_id = incoming.connection_id.clone();
        if let Some(peer_room) = hub
            .live_conn(room, &connection_id)
            .map(|c| c.peer_room.clone())
            && let Some(peer) = hub.rooms.get_mut(&peer_room)
            && let Some(out) = peer
                .outbox
                .iter_mut()
                .find(|o| o.exchange_id == request.exchange_id && !o.state.is_final())
        {
            out.state = request.state;
        }
        Ok(())
    }

    async fn ack(&self, room: &str, request: &AckRequest) -> Result<(), NetError> {
        let mut hub = self.hub.state.lock().unwrap();
        hub.calls.push(format!("{} ack {room}", self.user));
        let r = hub.owned(&self.user, room)?;
        let mut delivered = Vec::new();
        for id in &request.asks {
            if let Some(i) = r
                .inbox
                .iter_mut()
                .find(|i| &i.exchange_id == id && !i.acked)
            {
                i.acked = true;
                if i.state == WireState::Queued {
                    i.state = WireState::Delivered;
                    delivered.push((i.exchange_id.clone(), i.connection_id.clone()));
                }
            }
        }
        for ack in &request.states {
            if let Some(o) = r
                .outbox
                .iter_mut()
                .find(|o| o.exchange_id == ack.exchange_id && o.state == ack.state)
            {
                o.acked_state = Some(o.state);
            }
        }
        for id in &request.links {
            if let Some(c) = r.connections.iter_mut().find(|c| &c.id == id) {
                c.link_pending = None;
            }
        }
        for (exchange_id, connection_id) in delivered {
            if let Some(peer_room) = hub
                .live_conn(room, &connection_id)
                .map(|c| c.peer_room.clone())
                && let Some(peer) = hub.rooms.get_mut(&peer_room)
                && let Some(out) = peer
                    .outbox
                    .iter_mut()
                    .find(|o| o.exchange_id == exchange_id && !o.state.is_final())
            {
                out.state = WireState::Delivered;
            }
        }
        Ok(())
    }
}

fn expire(room: &mut Room, connection_id: &str) {
    for i in room
        .inbox
        .iter_mut()
        .filter(|i| i.connection_id == connection_id && !i.state.is_final())
    {
        i.state = WireState::Expired;
    }
    for o in room
        .outbox
        .iter_mut()
        .filter(|o| o.connection_id == connection_id && !o.state.is_final())
    {
        o.state = WireState::Expired;
        o.answered_at = Some(now());
    }
}
