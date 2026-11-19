//! The **Connections** card (§13.1; "network" in code): one row per
//! connected workspace, and in the panel one connection's page (its reply
//! mode, what it can read, and the questions between the two) or the
//! connect page (make an invite code, or use someone's).
//!
//! Bots connect through the owner's account (§11), so a signed-out device
//! shows a quiet "Sign in" state in place of the controls. Invites are
//! RPCs on the bot's host; an engine that does not have them yet answers
//! an unknown method, and the page says so instead of failing.

use super::panel::{PanelHead, PanelItem};
use super::parts::{self, CallError};
use super::*;
use crate::kit::theme::MonoStyled as _;
use cypher_proto::{
    BotConnection, BotExchange, BotInvite, ExchangeDirection, ExchangeState, PeerBot, ReplyMode,
};

/// Exchanges a connection's page shows.
const RECENT_EXCHANGES: usize = 8;

/// What a connection is doing, the most pressing thing first: what its row
/// shows at its right and its page says in its bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Standing {
    /// Being disconnected on both sides.
    Leaving,
    /// An answer waits for the owner's review.
    Review,
    /// The workspace is answering one of their questions.
    Answering,
    /// One of the workspace's questions waits for their answer.
    Asking,
    /// Replies are off: their questions are declined.
    Off,
    Idle,
}

impl Standing {
    /// Something is under way: its row always shows.
    fn live(self) -> bool {
        matches!(self, Self::Review | Self::Answering | Self::Asking)
    }
}

/// Whether a question is still on its way to an answer.
fn in_flight(exchange: &BotExchange) -> bool {
    match exchange.state {
        ExchangeState::Queued | ExchangeState::Delivered | ExchangeState::Working => true,
        // Reviewed here; its host has not sent or declined it yet.
        ExchangeState::AwaitingReview => exchange.review.is_some(),
        _ => false,
    }
}

/// How long a question that waits on the other side keeps its ring moving.
const FRESH_MS: i64 = 60_000;

/// Whether a question in flight shows as moving. Its ring goes round while
/// this workspace writes its answer, and for the first minute of a wait on
/// the other side (their answer to a question asked, the sending of a
/// reviewed one): that wait can last days, so after a minute the ring holds
/// still until the question settles.
fn moving(exchange: &BotExchange, now_ms: i64) -> bool {
    if !in_flight(exchange) {
        return false;
    }
    let since = match (&exchange.review, exchange.direction) {
        (Some(review), _) => review.at,
        (None, ExchangeDirection::Outgoing) => exchange.asked_at,
        (None, ExchangeDirection::Incoming) => return true,
    };
    now_ms - since < FRESH_MS
}

/// The ring of something in flight: going round, or still once its wait
/// has gone on ([`moving`]).
fn flight_ring(moving: bool, ticker: gpui::EntityId, theme: &Theme) -> AnyElement {
    super::items::ring_mark(
        if moving {
            super::items::RingState::Running
        } else {
            super::items::RingState::Waiting
        },
        (0, 0),
        ticker,
        theme,
    )
}

pub(super) fn standing<'a>(
    conn: &BotConnection,
    exchanges: impl IntoIterator<Item = &'a BotExchange>,
) -> Standing {
    if conn.revoke_requested_at.is_some() {
        return Standing::Leaving;
    }
    let (mut review, mut answering, mut asking) = (false, false, false);
    for exchange in exchanges.into_iter().filter(|e| e.connection_id == conn.id) {
        let incoming = exchange.direction == ExchangeDirection::Incoming;
        review |= exchange.awaiting_review();
        answering |= incoming && in_flight(exchange);
        asking |= !incoming && in_flight(exchange);
    }
    match () {
        _ if review => Standing::Review,
        _ if answering => Standing::Answering,
        _ if asking => Standing::Asking,
        _ if conn.reply_mode == ReplyMode::Off => Standing::Off,
        _ => Standing::Idle,
    }
}

/// Who a connection is: the person, and their workspace's name beside it;
/// the workspace alone when its owner is unnamed.
pub(super) fn peer_names(peer: &PeerBot) -> (String, Option<String>) {
    let owner = transcript::single_line(&peer.owner_name);
    let name = transcript::single_line(&peer.name);
    if owner.trim().is_empty() {
        (name, None)
    } else {
        (owner, Some(name))
    }
}

/// The first word of a person's name, for a short status ("Waiting for
/// Dana").
fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

/// What a connection's questions can read, after "can read": everything
/// the workspace reads, or the projects it is narrowed to ("billing,
/// web-api", "billing, web-api +2", "no projects"). `projects` are the
/// workspace's own, id and name.
pub(super) fn reads_words(
    scope: Option<&BotScope>,
    projects: &[(String, String)],
    bot: &str,
) -> String {
    let Some(scope) = scope else {
        return format!("everything {bot} reads");
    };
    let names: Vec<&str> = projects
        .iter()
        .filter(|(id, _)| scope.sees_space(id))
        .map(|(_, name)| name.as_str())
        .collect();
    match names.as_slice() {
        [] => "no projects".into(),
        names if names.len() == projects.len() => format!("everything {bot} reads"),
        [one] => (*one).to_string(),
        [one, two] => format!("{one}, {two}"),
        [one, two, rest @ ..] => format!("{one}, {two} +{}", rest.len()),
    }
}

/// What a question with no answer to show says once opened.
fn unanswered_words(exchange: &BotExchange, bot: &str) -> String {
    let incoming = exchange.direction == ExchangeDirection::Incoming;
    let decided = exchange.review.as_ref().map(|review| review.send);
    match (incoming, exchange.state, decided) {
        (true, ExchangeState::Declined, _) | (true, _, Some(false)) => "Declined.".into(),
        (true, ExchangeState::Expired, _) => "It expired before it was answered.".into(),
        (true, _, Some(true)) => "Sending your answer…".into(),
        (true, ExchangeState::Answered, _) => "Answered.".into(),
        (true, _, _) => format!("{bot} is writing its answer…"),
        (false, ExchangeState::Declined, _) => "They declined to answer.".into(),
        (false, ExchangeState::Expired, _) => "No answer came; it expired.".into(),
        (false, ExchangeState::Answered, _) => "Answered.".into(),
        (false, _, _) => "Waiting for their answer.".into(),
    }
}

/// How long an open invite has left, for a row's corner: "5d left", "3h
/// left", "soon".
fn left_words(expires_at: i64, now_ms: i64) -> String {
    let minutes = (expires_at - now_ms).max(0) / 60_000;
    match minutes {
        0..60 => "soon".into(),
        60..1440 => format!("{}h left", minutes / 60),
        _ => format!("{}d left", minutes / 1440),
    }
}

/// The bot's live connections, most recently active first.
pub(super) fn live_connections<'a>(
    connections: &'a [BotConnection],
    bot_id: &str,
) -> Vec<&'a BotConnection> {
    let mut live: Vec<&BotConnection> = connections
        .iter()
        .filter(|c| c.bot_id == bot_id && c.is_live())
        .collect();
    live.sort_by_key(|c| {
        std::cmp::Reverse((c.last_exchange_at.unwrap_or(c.created_at), c.id.clone()))
    });
    live
}

/// The bot's invites still open and not being revoked, newest first.
pub(super) fn open_invites<'a>(
    invites: &'a [BotInvite],
    bot_id: &str,
    now_ms: i64,
) -> Vec<&'a BotInvite> {
    let mut open: Vec<&BotInvite> = invites
        .iter()
        .filter(|i| i.bot_id == bot_id && i.is_open(now_ms))
        .collect();
    open.sort_by_key(|i| std::cmp::Reverse(i.created_at));
    open
}

/// A narrowed scope for a connection: the bot's own, limited to `space_ids`.
fn narrowed(bot: &BotScope, space_ids: Vec<String>) -> BotScope {
    BotScope {
        projects: ProjectGrant::Selected { space_ids },
        include_sessions: Vec::new(),
        ..bot.clone()
    }
}

/// An invite code just issued: shown once, never stored.
struct Issued {
    bot_id: String,
    code: String,
    expires_at: i64,
    copied: bool,
}

/// The network panels' own state.
#[derive(Default)]
pub(super) struct NetworkUi {
    issued: Option<Issued>,
    creating: bool,
    /// Why the last invite failed, and whether it is only "not yet" (said
    /// quietly) rather than an error.
    invite_error: Option<(SharedString, bool)>,
    redeem: Option<(Entity<TextInput>, Subscription)>,
    redeeming: bool,
    redeem_error: Option<(SharedString, bool)>,
    /// Questions opened on a connection's page (exchange ids).
    pub(super) opened: std::collections::HashSet<String>,
    /// The connection whose reply-mode menu is open, and the one whose
    /// "can read" editor is.
    mode_open: Option<String>,
    scope_open: Option<String>,
}

impl Shell {
    /// Whether this device can reach the bot network: signed in.
    fn network_ready(&self, cx: &App) -> bool {
        // Development builds (and the capture driver) can show the
        // signed-in panels against a local engine with seeded rows:
        // `CYPHER_DEV_NETWORK_PREVIEW=1`. Calls still fail without an Edge.
        #[cfg(any(debug_assertions, feature = "dev-capture"))]
        if cypher_env::var("DEV_NETWORK_PREVIEW").is_some() {
            return true;
        }
        matches!(
            self.state.read(cx).auth,
            Some(cypher_proto::AuthState::SignedIn { .. })
        )
    }

    fn redeem_input(&mut self, cx: &mut Context<Self>) -> Entity<TextInput> {
        if let Some((input, _)) = &self.board.network.redeem {
            return input.clone();
        }
        let input = cx.new(|cx| TextInput::settings_field("Paste a code", false, cx));
        let events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, TextInputEvent::Submitted) {
                this.redeem_invite(cx);
            }
        });
        self.board.network.redeem = Some((input.clone(), events));
        input
    }

    /// The words for a failed network call, and whether they only mean
    /// "not yet" (said quietly, not as an error).
    fn network_failure(&self, err: CallError, cx: &App) -> (SharedString, bool) {
        let quiet = err == CallError::Unknown;
        (self.network_words(err, cx), quiet)
    }

    fn network_words(&self, err: CallError, cx: &App) -> SharedString {
        let host = {
            let state = self.state.read(cx);
            current_bot(&state.bots, self.settings.botboard_bot.as_deref())
                .and_then(|bot| state.device_name(&bot.host_device_id))
                .unwrap_or("this workspace's device")
                .to_string()
        };
        match err {
            CallError::Unknown => {
                format!("Connecting workspaces isn't available on {host} yet.").into()
            }
            CallError::Failed(text) if text.to_lowercase().contains("sign") => {
                "Sign in to connect workspaces.".into()
            }
            CallError::Failed(text) => text.into(),
        }
    }

    /// Create an invite on the bot's host and show its code once.
    fn create_invite(&mut self, cx: &mut Context<Self>) {
        if self.board.network.creating {
            return;
        }
        let Some(bot) = ({
            let state = self.state.read(cx);
            current_bot(&state.bots, self.settings.botboard_bot.as_deref()).cloned()
        }) else {
            return;
        };
        let net = &mut self.board.network;
        net.creating = true;
        net.issued = None;
        net.invite_error = None;
        self.show_panel(PanelItem::Invite, cx);
        cx.notify();
        let bot_id = bot.id.clone();
        self.board_call(
            methods::CREATE_BOT_INVITE,
            serde_json::json!({ "botId": bot.id, "targetDeviceId": bot.host_device_id }),
            cx,
            move |this, result, cx| {
                this.board.network.creating = false;
                let issued = result
                    .map_err(|err| this.network_failure(err, cx))
                    .and_then(|v| {
                        let code = v.get("code").and_then(|c| c.as_str());
                        let expires_at = v.get("expiresAt").and_then(|e| e.as_i64());
                        match (code, expires_at) {
                            (Some(code), Some(expires_at)) => Ok(Issued {
                                bot_id,
                                code: code.to_string(),
                                expires_at,
                                copied: false,
                            }),
                            _ => Err(("The device sent back no code.".into(), false)),
                        }
                    });
                match issued {
                    Ok(issued) => this.board.network.issued = Some(issued),
                    Err(message) => this.board.network.invite_error = Some(message),
                }
            },
        );
    }

    /// Redeem another owner's code for the current bot.
    fn redeem_invite(&mut self, cx: &mut Context<Self>) {
        if self.board.network.redeeming {
            return;
        }
        let input = self.redeem_input(cx);
        let code = input.read(cx).text().trim().to_string();
        let Some(bot) = ({
            let state = self.state.read(cx);
            current_bot(&state.bots, self.settings.botboard_bot.as_deref()).cloned()
        }) else {
            return;
        };
        if code.is_empty() {
            self.board.network.redeem_error = Some(("Paste the code you were sent.".into(), false));
            return cx.notify();
        }
        self.board.network.redeeming = true;
        self.board.network.redeem_error = None;
        cx.notify();
        self.board_call(
            methods::REDEEM_BOT_INVITE,
            serde_json::json!({
                "botId": bot.id,
                "code": code,
                "targetDeviceId": bot.host_device_id,
            }),
            cx,
            move |this, result, cx| {
                this.board.network.redeeming = false;
                match result {
                    Ok(value) => {
                        input.update(cx, |input, cx| input.set_text("", cx));
                        // Its row comes with the records either way.
                        match serde_json::from_value::<BotConnection>(value) {
                            Ok(conn) => this.show_panel(PanelItem::Connection(conn.id), cx),
                            Err(_) => this.close_panel(cx),
                        }
                    }
                    Err(err) => {
                        this.board.network.redeem_error = Some(this.network_failure(err, cx))
                    }
                }
            },
        );
    }

    /// Open the connect page (the card's +, its hint, an open invite's row).
    pub(super) fn open_connect(&mut self, cx: &mut Context<Self>) {
        let net = &mut self.board.network;
        net.invite_error = None;
        net.redeem_error = None;
        self.show_panel(PanelItem::Invite, cx);
    }

    /// The sidebar card: one line per connected workspace, like the other
    /// cards. Who it is, and at the right the one thing that matters most
    /// about it now ([`Standing`]): an amber mark when an answer waits for
    /// review, the ring while a question is on its way, else when the last
    /// question was, or that replies are off. An open invite is one quiet
    /// last row. With nothing yet, the header alone says how to start.
    pub(super) fn render_network_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = Utc::now();
        let now_ms = now.timestamp_millis();
        let (rows, invites, moving) = {
            let state = self.state.read(cx);
            let records = &state.bot_records;
            let mut rows: Vec<(BotConnection, Standing)> =
                live_connections(&records.connections, &bot.id)
                    .into_iter()
                    .map(|conn| (conn.clone(), standing(conn, &records.exchanges)))
                    .collect();
            // The connections with a question whose ring still goes round.
            let moving: std::collections::HashSet<String> = records
                .exchanges
                .iter()
                .filter(|exchange| moving(exchange, now_ms))
                .map(|exchange| exchange.connection_id.clone())
                .collect();
            // What waits on the owner first, then what is on its way; the
            // rest stay by last question.
            rows.sort_by_key(|(_, standing)| match standing {
                Standing::Review => 0,
                Standing::Answering | Standing::Asking => 1,
                _ => 2,
            });
            let invites: Vec<i64> = open_invites(&records.invites, &bot.id, now_ms)
                .iter()
                .map(|invite| invite.expires_at)
                .collect();
            (rows, invites, moving)
        };
        if rows.is_empty() && invites.is_empty() {
            let key = "bot-network-card";
            let header = super::board::header_frame(
                key,
                "Connections",
                self.card_focused("network", cx),
                theme,
            )
            .cursor_pointer()
            .on_hover(motion::hover_listener(key))
            .on_click(cx.listener(|this, _, _, cx| this.open_connect(cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted.opacity(0.6))
                    .child(if self.network_ready(cx) {
                        "Connect a workspace"
                    } else {
                        "Sign in to connect"
                    }),
            );
            return Some(super::board::card(theme).child(header).into_any_element());
        }
        let header = self.card_header(
            "network",
            "bot-network-header",
            "Connections",
            rows.len(),
            Some(super::board::HeaderAction::Connect),
            None,
            theme,
            cx,
        );
        let live = |(_, standing): &(BotConnection, Standing)| standing.live();
        let (rows, hidden) = if self.list_expanded("network") {
            let hidden = super::board::short_rows(rows.iter().map(live).collect(), |l| *l).1;
            (rows, hidden)
        } else {
            super::board::short_rows(rows, live)
        };
        let open = self.board.panel.clone();
        let ticker = self.pulse_ticker.entity_id();
        let mut items: Vec<AnyElement> = rows
            .iter()
            .map(|(conn, standing)| {
                let item = PanelItem::Connection(conn.id.clone());
                let corner = match standing {
                    Standing::Review => icon(icons::QUESTION_CIRCLE)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.warning)
                        .into_any_element(),
                    Standing::Answering | Standing::Asking => {
                        flight_ring(moving.contains(&conn.id), ticker, theme)
                    }
                    Standing::Leaving => super::board::card_corner("leaving", None, theme),
                    Standing::Off => super::board::card_corner("off", None, theme),
                    Standing::Idle => super::board::card_corner(
                        conn.last_exchange_at
                            .map(|at| super::items::ago(at, now))
                            .unwrap_or_default(),
                        None,
                        theme,
                    ),
                };
                let (who, theirs) = peer_names(&conn.peer);
                let dim = matches!(standing, Standing::Off | Standing::Leaving);
                let selected = open.as_ref() == Some(&item);
                super::board::card_item_aside(
                    &format!("bot-connection-{}", conn.id),
                    Some(peer_avatar(&conn.peer, 14.0, theme)),
                    who,
                    theirs,
                    Some(corner),
                    selected,
                    theme,
                )
                .when(dim, |el| el.opacity(0.55))
                .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
                .into_any_element()
            })
            .collect();
        items.extend(self.more_row("network".into(), hidden, theme, cx));
        // An unused code is an open door: it keeps a row while it is open.
        if let Some(soonest) = invites.iter().min() {
            items.push(
                super::board::card_item(
                    "bot-network-invites",
                    Some(super::board::card_glyph(
                        icons::KEY_MINIMALISTIC,
                        theme.text_muted.opacity(0.82),
                    )),
                    match invites.len() {
                        1 => "Invite open".to_string(),
                        n => format!("{n} invites open"),
                    },
                    Some(super::board::card_corner(
                        left_words(*soonest, now_ms),
                        None,
                        theme,
                    )),
                    open == Some(PanelItem::Invite),
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.open_connect(cx)))
                .into_any_element(),
            );
        }
        Some(self.fold_card("network", header, items, theme))
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_network_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        // The code is shown once: leaving the connect page drops it.
        if *item != PanelItem::Invite && self.board.network.issued.is_some() {
            self.board.network.issued = None;
            self.board.network.invite_error = None;
        }
        match item {
            PanelItem::Invite => Some(self.render_invite_panel(bot, theme, cx)),
            PanelItem::Connection(id) => self.render_connection(bot, id, theme, window, cx),
            _ => None,
        }
    }

    /// An open invite on the connect page: when it was made, how long it
    /// has left, and Revoke.
    fn invite_row(
        &self,
        invite: &BotInvite,
        ready: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = invite.id.clone();
        let revoking = invite.revoke_requested_at.is_some();
        let mut note = if revoking {
            "revoking…".to_string()
        } else {
            left_words(invite.expires_at, cypher_proto::now_ms())
        };
        // Only a code that works more than once says how often it was used.
        if invite.max_uses > 1 && !revoking {
            note = format!("{} of {} used · {note}", invite.uses, invite.max_uses);
        }
        div()
            .mx(px(8.0))
            .h(px(30.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_size(px(12.0))
            .child(
                icon(icons::KEY_MINIMALISTIC)
                    .size(px(13.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text.opacity(0.85))
                    .child(SharedString::from(format!(
                        "Made {}",
                        parts::ago_ms(invite.created_at)
                    ))),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(theme.text_muted.opacity(0.85))
                    .child(SharedString::from(note)),
            )
            .when(ready && !revoking, |el| {
                el.child(
                    parts::action(
                        format!("bot-invite-revoke-{}", invite.id),
                        icons::CLOSE,
                        "Revoke",
                        theme.text_muted,
                    )
                    .mr(px(-9.0))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board_mutate(
                            serde_json::json!({ "op": "revokeBotInvite", "inviteId": id }),
                            cx,
                        );
                    })),
                )
            })
    }

    /// The connect page: make an invite code (and see the ones still
    /// open), or use someone else's. What crosses Cypher's servers is said
    /// here, once, where connecting is decided.
    fn render_invite_panel(
        &mut self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let head = PanelHead {
            glyph: icons::LINK,
            tint: None,
            title: "Connect a workspace".into(),
        };
        let ready = self.network_ready(cx);
        let invites: Vec<BotInvite> = {
            let state = self.state.read(cx);
            open_invites(&state.bot_records.invites, &bot.id, cypher_proto::now_ms())
                .into_iter()
                .cloned()
                .collect()
        };
        if !ready {
            let mut body = div().p(px(8.0)).flex().flex_col().child(signed_out(theme));
            for invite in &invites {
                body = body.child(self.invite_row(invite, false, theme, cx));
            }
            return (head, body.child(privacy_note(theme)));
        }
        let input = self.redeem_input(cx);
        let net = &self.board.network;
        let issued = net
            .issued
            .as_ref()
            .filter(|i| i.bot_id == bot.id)
            .map(|i| (i.code.clone(), i.expires_at, i.copied));
        let invite_body = match issued {
            Some((code, expires_at, copied)) => {
                let copy_code = code.clone();
                div()
                    .mx(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .pl(px(12.0))
                            .pr(px(4.0))
                            .py(px(4.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(crate::kit::theme::hairline(0.08))
                            .bg(crate::kit::theme::ink(0.04))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .mono(theme)
                                    .text_size(px(12.5))
                                    .text_color(theme.text)
                                    .child(SharedString::from(code)),
                            )
                            .child(
                                parts::action(
                                    "bot-invite-copy",
                                    if copied { icons::CHECK } else { icons::COPY },
                                    if copied { "Copied" } else { "Copy" },
                                    theme.text,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            copy_code.clone(),
                                        ));
                                        if let Some(issued) = this.board.network.issued.as_mut() {
                                            issued.copied = true;
                                        }
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!(
                                "Shown only this once. Works once, expires {}.",
                                expiry_words(expires_at)
                            ))),
                    )
            }
            None => div()
                .mx(px(8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .child(
                    popover::btn_primary(
                        theme,
                        if net.creating {
                            "Creating…"
                        } else {
                            "Create invite code"
                        },
                    )
                    .id("bot-invite-create")
                    .py(px(5.0))
                    .when(net.creating, |el| el.opacity(0.6))
                    .on_click(cx.listener(|this, _, _, cx| this.create_invite(cx))),
                )
                .children(net.invite_error.clone().map(|(error, quiet)| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(if quiet {
                            theme.text_muted
                        } else {
                            theme.danger
                        })
                        .child(error)
                })),
        };
        let redeem = div()
            .mx(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(parts::field(&input).mx(px(0.0)).flex_1().min_w_0())
            .child(
                popover::btn_primary(
                    theme,
                    if net.redeeming {
                        "Connecting…"
                    } else {
                        "Connect"
                    },
                )
                .id("bot-invite-redeem")
                .py(px(5.0))
                .when(net.redeeming, |el| el.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.redeem_invite(cx))),
            );
        let redeem_error = net.redeem_error.clone();
        let mut body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(parts::section_label(theme, "Invite someone"))
            .child(invite_body);
        if !invites.is_empty() {
            body = body.child(
                div().pt(px(6.0)).flex().flex_col().children(
                    invites
                        .iter()
                        .map(|invite| self.invite_row(invite, true, theme, cx)),
                ),
            );
        }
        body = body
            .child(parts::section_label(theme, "Use their code"))
            .child(redeem)
            .children(redeem_error.map(|(error, quiet)| {
                div()
                    .px(px(8.0))
                    .pt(px(6.0))
                    .text_size(px(11.5))
                    .text_color(if quiet {
                        theme.text_muted
                    } else {
                        theme.danger
                    })
                    .child(error)
            }))
            .child(privacy_note(theme));
        (head, body)
    }

    /// One connection's page, laid out like a work item's: every control at
    /// the very top beside a word of how it stands, the person, one line of
    /// which workspace it is and what it can read, then only the questions
    /// between the two, newest first.
    pub(super) fn render_connection(
        &mut self,
        bot: &Bot,
        id: &str,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let ready = self.network_ready(cx);
        let (conn, exchanges, projects, thread_listed) = {
            let state = self.state.read(cx);
            let records = &state.bot_records;
            let conn = records
                .connections
                .iter()
                .find(|c| c.id == id && c.bot_id == bot.id)
                .cloned()?;
            let mut exchanges: Vec<BotExchange> = records
                .exchanges
                .iter()
                .filter(|e| e.connection_id == id)
                .cloned()
                .collect();
            exchanges.sort_by_key(|e| std::cmp::Reverse(e.asked_at));
            exchanges.truncate(RECENT_EXCHANGES);
            let projects: Vec<(String, String)> = state
                .spaces_sorted()
                .into_iter()
                .filter(|s| bot.scope.sees_space(&s.id))
                .map(|s| (s.id.clone(), s.display_name().to_string()))
                .collect();
            let thread_listed = conn
                .thread_chat_id
                .as_deref()
                .is_some_and(|id| state.chats.iter().any(|c| c.id == id));
            (conn, exchanges, projects, thread_listed)
        };
        let (who, theirs) = peer_names(&conn.peer);
        let head = PanelHead {
            glyph: icons::LINK,
            tint: None,
            title: who.clone(),
        };
        let revoked = conn.revoked_at.is_some();
        let editable = ready && !revoked && conn.revoke_requested_at.is_none();
        let net = &self.board.network;
        let mode_open = editable && net.mode_open.as_deref() == Some(id);
        let scope_open = editable && net.scope_open.as_deref() == Some(id);

        // ── the bar: how it stands, then every control ──
        let standing = standing(&conn, &exchanges);
        let ticker = self.pulse_ticker.entity_id();
        let ring = |state| super::items::ring_mark(state, (0, 0), ticker, theme);
        let now_ms = cypher_proto::now_ms();
        let flight = || {
            flight_ring(
                exchanges.iter().any(|exchange| moving(exchange, now_ms)),
                ticker,
                theme,
            )
        };
        let (mark, word, tone) = match standing {
            _ if revoked => (
                peer_avatar(&conn.peer, 16.0, theme),
                "Disconnected".to_string(),
                theme.text_muted,
            ),
            Standing::Leaving => (
                ring(super::items::RingState::Stopping),
                "Disconnecting…".to_string(),
                theme.text_muted,
            ),
            Standing::Review => (
                icon(icons::QUESTION_CIRCLE)
                    .size(px(13.0))
                    .text_color(theme.warning)
                    .into_any_element(),
                "Waiting on you".to_string(),
                theme.warning,
            ),
            Standing::Answering => (flight(), "Answering".to_string(), theme.text),
            Standing::Asking => (
                flight(),
                format!("Waiting for {}", first_name(&who)),
                theme.text,
            ),
            Standing::Off => (
                peer_avatar(&conn.peer, 16.0, theme),
                "Replies off".to_string(),
                theme.text_muted,
            ),
            Standing::Idle => (
                peer_avatar(&conn.peer, 16.0, theme),
                "Connected".to_string(),
                theme.text,
            ),
        };
        let since = SharedString::from(format!(
            "Connected {}",
            parts::past_full_ms(conn.created_at)
        ));
        let status = super::items::header_status(format!("bot-conn-status-{id}"), mark, word, tone)
            .tooltip(move |_, cx| {
                let since = since.clone();
                cx.new(|_| super::super::session::FindTooltip(since)).into()
            });
        let mut controls: Vec<AnyElement> = Vec::new();
        if editable {
            controls.push(self.reply_mode_control(&conn, mode_open, theme, cx));
        }
        if thread_listed {
            let thread = PanelItem::Thread(conn.thread_chat_id.clone().unwrap_or_default());
            controls.push(
                super::items::icon_button(
                    "bot-conn-thread",
                    icons::CHAT_ROUND_LINE,
                    "Open its thread: where their questions are answered",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_panel(thread.clone(), cx)))
                .into_any_element(),
            );
        }
        if editable {
            let (revoke_id, name) = (conn.id.clone(), who.clone());
            controls.push(
                super::items::icon_button(
                    "bot-conn-revoke",
                    icons::TRASH_BIN_MINIMALISTIC,
                    "Disconnect",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.ask_confirm(
                        parts::Confirm::RevokeConnection {
                            id: revoke_id.clone(),
                            name: name.clone(),
                        },
                        cx,
                    )
                }))
                .into_any_element(),
            );
        }

        // ── under the name: which workspace, and what it can read ──
        let reads = format!(
            "can read {}",
            reads_words(conn.scope.as_ref(), &projects, &bot.name)
        );
        let line_text = match &theirs {
            Some(theirs) => format!("{theirs} · {reads}"),
            None => reads,
        };
        let line_key = format!("bot-conn-reads-{id}");
        let toggle_id = id.to_string();
        let line = div()
            .id(SharedString::from(line_key.clone()))
            .min_w_0()
            .ml(px(-5.0))
            .px(px(5.0))
            .h(px(20.0))
            .rounded(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.0))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(line_text)),
            )
            // Changing what it can read opens in place, under the header.
            .when(editable && !projects.is_empty(), |el| {
                el.cursor_pointer()
                    .text_color(motion::hover_blend(&line_key, theme.text_muted, theme.text))
                    .bg(if scope_open {
                        crate::kit::theme::wash(0.07)
                    } else {
                        motion::hover_blend(
                            &line_key,
                            crate::kit::theme::wash(0.0),
                            crate::kit::theme::wash(0.07),
                        )
                    })
                    .on_hover(motion::hover_listener(line_key.clone()))
                    .tooltip(|_, cx| {
                        cx.new(|_| {
                            super::super::session::FindTooltip("Change what it can read".into())
                        })
                        .into()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let net = &mut this.board.network;
                        net.scope_open = match net.scope_open.take() {
                            Some(open) if open == toggle_id => None,
                            _ => Some(toggle_id.clone()),
                        };
                        cx.notify();
                    }))
                    .child(
                        icon(if scope_open {
                            icons::ALT_ARROW_DOWN
                        } else {
                            icons::PEN
                        })
                        .size(px(10.0))
                        .flex_none()
                        .text_color(theme.text_faint.opacity(if scope_open {
                            1.0
                        } else {
                            motion::hover_t(&line_key)
                        })),
                    )
            });
        self.board.panel_header = Some(super::items::item_header(
            status,
            controls,
            super::items::header_title(format!("bot-conn-title-{id}"), &who, theme),
            Some(line.into_any_element()),
            theme,
        ));

        let mut body = div().p(px(8.0)).pb(px(14.0)).flex().flex_col();
        if scope_open {
            body = body.child(self.scope_editor(bot, &conn, &projects, theme, cx));
        }
        if exchanges.is_empty() {
            body = body.child(parts::quiet_line(theme, "No questions yet."));
        }
        let bot_name = bot.name.clone();
        for exchange in &exchanges {
            body = body.child(self.exchange_item(exchange, &who, &bot_name, theme, window, cx));
        }
        Some((head, body))
    }

    /// The reply mode in the bar: a button naming the mode, opening the
    /// three to choose from with what each does.
    fn reply_mode_control(
        &self,
        conn: &BotConnection,
        open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = |mode| match mode {
            ReplyMode::Auto => "Auto",
            ReplyMode::Review => "Review",
            ReplyMode::Off => "Off",
        };
        let menu = open.then(|| {
            let rows = [
                (
                    ReplyMode::Auto,
                    "Answers go back as soon as they are written.",
                ),
                (ReplyMode::Review, "Answers wait for you in Needs you."),
                (ReplyMode::Off, "Their questions are declined."),
            ]
            .map(|(mode, detail)| {
                let on = mode == conn.reply_mode;
                popover::menu_row(theme, on, format!("bot-conn-mode-{}", label(mode)))
                    .id(SharedString::from(format!("bot-conn-mode-{}", label(mode))))
                    .items_start()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board.network.mode_open = None;
                        if !on {
                            this.update_connection(None, Some(mode), None, cx);
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.0))
                            .child(div().text_color(theme.text).child(label(mode)))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(15.0))
                                    .text_color(theme.text_muted)
                                    .child(detail),
                            ),
                    )
                    .child(div().w(px(14.0)).flex_none().pt(px(3.0)).when(on, |el| {
                        el.child(icon(icons::CHECK).size(px(12.0)).text_color(theme.text))
                    }))
            });
            let card = popover::popover_card(theme)
                .w(px(250.0))
                .p(px(4.0))
                .flex()
                .flex_col()
                .gap(px(1.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.board.network.mode_open = None;
                    cx.notify();
                }))
                .children(rows);
            popover::anchored_menu_below_end("bot-conn-mode-menu", card.into_any_element(), None)
        });
        let key = "bot-conn-mode";
        let id = conn.id.clone();
        div()
            .id(key)
            .relative()
            .flex_none()
            .h(px(26.0))
            .pl(px(9.0))
            .pr(px(7.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.0))
            .cursor_pointer()
            .text_size(px(12.5))
            .text_color(theme.text)
            .bg(if open {
                crate::kit::theme::ink(0.07)
            } else {
                motion::hover_blend(
                    key,
                    crate::kit::theme::ink(0.02),
                    crate::kit::theme::ink(0.07),
                )
            })
            .on_hover(motion::hover_listener(key))
            .when(!open, |el| {
                el.tooltip(|_, cx| {
                    cx.new(|_| {
                        super::super::session::FindTooltip(
                            "How their questions are answered".into(),
                        )
                    })
                    .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board.network.mode_open = Some(id.clone());
                    cx.notify();
                }))
            })
            .child(label(conn.reply_mode))
            .child(
                icon(icons::ALT_ARROW_DOWN)
                    .size(px(9.0))
                    .text_color(theme.text_muted),
            )
            .children(menu)
            .into_any_element()
    }

    /// What a connection can read, to change in place under its header:
    /// everything the workspace reads, or only the projects ticked.
    fn scope_editor(
        &self,
        bot: &Bot,
        conn: &BotConnection,
        projects: &[(String, String)],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let narrowed_ids: Option<Vec<String>> = conn.scope.as_ref().map(|s| match &s.projects {
            ProjectGrant::Selected { space_ids } => space_ids.clone(),
            ProjectGrant::All => projects.iter().map(|(id, _)| id.clone()).collect(),
            ProjectGrant::None => Vec::new(),
        });
        let same_label = format!("All {} reads", bot.name);
        let mode = if narrowed_ids.is_some() { 1usize } else { 0 };
        let mut editor = div()
            .mx(px(4.0))
            .mb(px(10.0))
            .p(px(6.0))
            .rounded(px(10.0))
            .bg(crate::kit::theme::ink(0.03))
            .flex()
            .flex_col()
            .child(parts::segmented(
                "bot-conn-scope",
                &[
                    (0usize, same_label.as_str()),
                    (1usize, "Only some projects"),
                ],
                mode,
                |this, pick, cx| this.update_connection(Some(pick == 1), None, None, cx),
                theme,
                cx,
            ));
        if let Some(ids) = &narrowed_ids {
            let rows = projects.iter().map(|(space_id, name)| {
                let on = ids.contains(space_id);
                let pick = space_id.clone();
                div()
                    .id(SharedString::from(format!("bot-conn-project-{space_id}")))
                    .h(px(28.0))
                    .px(px(6.0))
                    .rounded(px(7.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(9.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_connection(None, None, Some(pick.clone()), cx)
                    }))
                    .child(check_box(on, theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(theme.text)
                            .child(SharedString::from(name.clone())),
                    )
            });
            editor = editor.child(
                div()
                    .mt(px(4.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .children(rows),
            );
        }
        editor
    }

    /// One question on a connection's page: which way it went, the
    /// question, and at the right how it stands (an amber mark when its
    /// answer waits for review, the ring while it is on its way, that it
    /// was declined or expired) or just when it was asked. A click opens
    /// its answer in place; one waiting for review opens the review.
    fn exchange_item(
        &self,
        exchange: &BotExchange,
        who: &str,
        bot: &str,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let now = Utc::now();
        let incoming = exchange.direction == ExchangeDirection::Incoming;
        let awaiting = exchange.awaiting_review();
        let open = !awaiting && self.board.network.opened.contains(&exchange.id);
        let ended = matches!(
            exchange.state,
            ExchangeState::Declined | ExchangeState::Expired
        );
        let asked = super::items::ago(exchange.asked_at, now);
        let note: AnyElement = if awaiting {
            icon(icons::QUESTION_CIRCLE)
                .size(px(12.0))
                .text_color(theme.warning)
                .into_any_element()
        } else if in_flight(exchange) {
            flight_ring(
                moving(exchange, now.timestamp_millis()),
                self.pulse_ticker.entity_id(),
                theme,
            )
        } else {
            let text = match exchange.state {
                ExchangeState::Declined => format!("declined · {asked}"),
                ExchangeState::Expired => format!("expired · {asked}"),
                _ => asked,
            };
            super::items::trailing(text, theme.text_faint).into_any_element()
        };
        let times = SharedString::from(match exchange.answered_at {
            Some(answered) => format!(
                "Asked {} · answered {}",
                parts::past_full_ms(exchange.asked_at),
                parts::past_full_ms(answered)
            ),
            None => format!("Asked {}", parts::past_full_ms(exchange.asked_at)),
        });
        let direction = SharedString::from(if incoming {
            format!("{who} asked {bot}")
        } else {
            format!("{bot} asked {who}")
        });
        let key = format!("bot-conn-question-{}", exchange.id);
        let question = div()
            .text_size(px(12.5))
            .line_height(px(18.0))
            .text_color(if ended { theme.text_muted } else { theme.text });
        let mut content = div().flex_1().min_w_0().flex().flex_col();
        content = if open {
            // Its answer as it went (the owner's edit, when there was one).
            let answer = exchange
                .review
                .as_ref()
                .and_then(|review| review.text.clone())
                .or_else(|| exchange.answer.clone())
                .filter(|answer| !answer.trim().is_empty() && !in_flight(exchange) && !ended);
            content
                .child(question.child(SharedString::from(exchange.question.trim().to_string())))
                .child(
                    div().pt(px(5.0)).child(match answer {
                        Some(answer) => super::items::panel_markdown(
                            &answer,
                            format!("{key}-answer"),
                            12.5,
                            theme.text_muted,
                            theme,
                            window,
                        ),
                        None => div()
                            .text_size(px(12.0))
                            .line_height(px(17.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(unanswered_words(exchange, bot)))
                            .into_any_element(),
                    }),
                )
        } else {
            content.child(
                question
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(SharedString::from(transcript::single_line(
                        &exchange.question,
                    ))),
            )
        };
        let (id, review) = (exchange.id.clone(), exchange.id.clone());
        div()
            .id(SharedString::from(key.clone()))
            .mx(px(2.0))
            .px(px(6.0))
            .py(px(7.0))
            .rounded(px(10.0))
            .flex()
            .flex_row()
            .items_start()
            .gap(px(9.0))
            .cursor_pointer()
            .bg(if open {
                crate::kit::theme::wash(0.05)
            } else {
                motion::hover_blend(
                    &key,
                    crate::kit::theme::wash(0.0),
                    crate::kit::theme::wash(0.05),
                )
            })
            .on_hover(motion::hover_listener(key.clone()))
            .on_click(cx.listener(move |this, _, _, cx| {
                if awaiting {
                    return this.open_panel(PanelItem::Review(review.clone()), cx);
                }
                let opened = &mut this.board.network.opened;
                if !opened.remove(&id) {
                    opened.insert(id.clone());
                }
                cx.notify();
            }))
            .child(
                div()
                    .id(SharedString::from(format!("{key}-way")))
                    .w(px(14.0))
                    .h(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .tooltip(move |_, cx| {
                        let direction = direction.clone();
                        cx.new(|_| super::super::session::FindTooltip(direction))
                            .into()
                    })
                    .child(
                        icon(if incoming {
                            icons::ARROW_DOWN
                        } else {
                            icons::ARROW_UP
                        })
                        .size(px(11.0))
                        .text_color(theme.text_muted.opacity(0.8)),
                    ),
            )
            .child(content)
            .child(
                div()
                    .id(SharedString::from(format!("{key}-note")))
                    .flex_none()
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .tooltip(move |_, cx| {
                        let times = times.clone();
                        cx.new(|_| super::super::session::FindTooltip(times)).into()
                    })
                    .child(note),
            )
    }

    /// Change the open connection: narrow or widen its scope, its reply
    /// mode, or flip one project in a narrowed scope. The whole owner
    /// state goes out (the engine writes the scope as given).
    fn update_connection(
        &mut self,
        narrow: Option<bool>,
        mode: Option<ReplyMode>,
        flip: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(PanelItem::Connection(id)) = self.board.panel.clone() else {
            return;
        };
        let (conn, bot, projects) = {
            let state = self.state.read(cx);
            let Some(conn) = state
                .bot_records
                .connections
                .iter()
                .find(|c| c.id == id)
                .cloned()
            else {
                return;
            };
            let Some(bot) = state.bots.iter().find(|b| b.id == conn.bot_id).cloned() else {
                return;
            };
            let projects: Vec<String> = state
                .spaces_sorted()
                .into_iter()
                .filter(|s| bot.scope.sees_space(&s.id))
                .map(|s| s.id.clone())
                .collect();
            (conn, bot, projects)
        };
        let mut scope = conn.scope.clone();
        if let Some(narrow) = narrow {
            scope = narrow.then(|| narrowed(&bot.scope, projects.clone()));
        }
        if let (Some(space_id), Some(current)) = (flip, scope.as_ref()) {
            let mut ids = match &current.projects {
                ProjectGrant::Selected { space_ids } => space_ids.clone(),
                ProjectGrant::All => projects.clone(),
                ProjectGrant::None => Vec::new(),
            };
            if let Some(ix) = ids.iter().position(|s| *s == space_id) {
                ids.remove(ix);
            } else {
                ids.push(space_id);
            }
            scope = Some(narrowed(&bot.scope, ids));
        }
        self.board_mutate(
            serde_json::json!({
                "op": "updateBotConnection",
                "connectionId": conn.id,
                "replyMode": mode.unwrap_or(conn.reply_mode),
                "scope": scope,
            }),
            cx,
        );
    }
}

/// "in 6 days", "tomorrow", "today", or the date.
fn expiry_words(at: i64) -> String {
    let Some(at) = parts::local(at) else {
        return String::new();
    };
    let now = chrono::Local::now();
    match (at.date_naive() - now.date_naive()).num_days() {
        ..=0 => format!("today {}", at.format("%H:%M")),
        1 => "tomorrow".into(),
        n @ 2..=13 => format!("in {n} days"),
        _ => at.format("%b %-d").to_string(),
    }
}

/// The person behind a connected workspace: their profile photo, round,
/// or their initial on a neutral disc while it loads, when it fails, or
/// when they have none (the workspace's initial when the owner is
/// unnamed).
pub(super) fn peer_avatar(peer: &PeerBot, size: f32, theme: &Theme) -> AnyElement {
    let initial = SharedString::from(crate::appearance::bot_style::initial(
        if peer.owner_name.trim().is_empty() {
            &peer.name
        } else {
            &peer.owner_name
        },
    ));
    let letter = {
        let theme = theme.clone();
        move || {
            div()
                .size(px(size))
                .flex_none()
                .rounded_full()
                .bg(crate::kit::theme::ink(0.08))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px((size * 0.52).round()))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(initial.clone())
                .into_any_element()
        }
    };
    // The engine keeps only avatar-host URLs; this is the UI's own guard
    // before gpui fetches one.
    match crate::shell::safe_avatar_url(peer.owner_avatar_url.clone().map(SharedString::from)) {
        Some(url) => gpui::img(url)
            .size(px(size))
            .flex_none()
            .rounded_full()
            .object_fit(gpui::ObjectFit::Cover)
            .with_loading(letter.clone())
            .with_fallback(letter)
            .into_any_element(),
        None => letter(),
    }
}

fn check_box(on: bool, theme: &Theme) -> gpui::Div {
    div()
        .size(px(15.0))
        .flex_none()
        .rounded(px(4.0))
        .border_1()
        .border_color(if on {
            theme.text
        } else {
            theme.text_muted.opacity(0.45)
        })
        .when(on, |el| el.bg(theme.text))
        .flex()
        .items_center()
        .justify_center()
        .when(on, |el| {
            el.child(icon(icons::CHECK).size(px(11.0)).text_color(theme.on_solid))
        })
}

/// The quiet state in place of the controls while signed out.
fn signed_out(theme: &Theme) -> gpui::Div {
    div()
        .mx(px(8.0))
        .mt(px(4.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(10.0))
        .bg(crate::kit::theme::ink(0.03))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(10.0))
        .child(
            icon(icons::CLOUD_OFF)
                .size(px(15.0))
                .mt(px(1.0))
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child("Sign in to connect workspaces"),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(theme.text_muted)
                        .child("Use the account menu. Connections go through your Cypher account."),
                ),
        )
}

/// §11.1 and §11.6: what crosses the Edge, said plainly.
fn privacy_note(theme: &Theme) -> gpui::Div {
    div()
        .px(px(8.0))
        .pt(px(16.0))
        .pb(px(4.0))
        .text_size(px(11.0))
        .line_height(px(15.0))
        .text_color(theme.text_faint)
        .child(
            "Questions and answers pass through Cypher's servers as plain text until \
             delivered. Likely secrets are removed from answers before they leave, \
             without guarantee.",
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::PeerBot;

    fn conn(id: &str, last: Option<i64>, created: i64) -> BotConnection {
        BotConnection {
            id: id.into(),
            bot_id: "b".into(),
            peer: PeerBot {
                room_id: "r".into(),
                name: "Ada".into(),
                owner_name: "Alice".into(),
                owner_avatar_url: None,
            },
            created_at: created,
            reply_mode: ReplyMode::Auto,
            scope: None,
            revoke_requested_at: None,
            revoked_at: None,
            thread_chat_id: None,
            last_exchange_at: last,
        }
    }

    fn invite(id: &str, created: i64, expires: i64) -> BotInvite {
        BotInvite {
            id: id.into(),
            bot_id: "b".into(),
            created_at: created,
            expires_at: expires,
            uses: 0,
            max_uses: 1,
            revoke_requested_at: None,
            revoked_at: None,
        }
    }

    fn exchange(id: &str, direction: ExchangeDirection, state: ExchangeState) -> BotExchange {
        BotExchange {
            id: id.into(),
            bot_id: "b".into(),
            connection_id: "c".into(),
            direction,
            question: "?".into(),
            answer: None,
            state,
            asked_at: 0,
            answered_at: None,
            review: None,
        }
    }

    #[test]
    fn a_connection_stands_by_its_most_pressing_question() {
        use ExchangeDirection::{Incoming, Outgoing};
        let mut c = conn("c", None, 0);
        let of = |c: &BotConnection, list: &[BotExchange]| standing(c, list);
        assert_eq!(of(&c, &[]), Standing::Idle);
        let answered = exchange("a", Incoming, ExchangeState::Answered);
        assert_eq!(of(&c, std::slice::from_ref(&answered)), Standing::Idle);
        let asked = exchange("o", Outgoing, ExchangeState::Delivered);
        assert_eq!(of(&c, &[answered.clone(), asked.clone()]), Standing::Asking);
        let working = exchange("w", Incoming, ExchangeState::Working);
        assert_eq!(
            of(&c, &[asked.clone(), working.clone()]),
            Standing::Answering
        );
        let mut draft = exchange("d", Incoming, ExchangeState::AwaitingReview);
        assert_eq!(
            of(&c, &[asked.clone(), working, draft.clone()]),
            Standing::Review
        );
        // Reviewed: it is on its way, no longer the owner's to decide.
        draft.review = Some(cypher_proto::ReviewDecision {
            send: true,
            text: None,
            at: 1,
        });
        assert_eq!(of(&c, std::slice::from_ref(&draft)), Standing::Answering);
        // Another connection's questions do not count.
        let mut other = exchange("x", Incoming, ExchangeState::Working);
        other.connection_id = "elsewhere".into();
        assert_eq!(of(&c, &[other]), Standing::Idle);
        c.reply_mode = ReplyMode::Off;
        assert_eq!(of(&c, &[]), Standing::Off);
        assert_eq!(of(&c, std::slice::from_ref(&asked)), Standing::Asking);
        c.revoke_requested_at = Some(1);
        assert_eq!(of(&c, &[asked]), Standing::Leaving);
        assert!(Standing::Review.live() && Standing::Asking.live());
        assert!(!Standing::Off.live() && !Standing::Idle.live());
    }

    #[test]
    fn a_wait_on_the_other_side_moves_for_a_minute_then_holds_still() {
        use ExchangeDirection::{Incoming, Outgoing};
        let minute = 60_000;
        // Asked at 0: their answer may take days.
        let asked = exchange("o", Outgoing, ExchangeState::Delivered);
        assert!(moving(&asked, 0) && moving(&asked, minute - 1));
        assert!(!moving(&asked, minute) && !moving(&asked, 3 * 24 * 60 * minute));
        // This workspace writing its answer is work under way: it moves.
        let working = exchange("w", Incoming, ExchangeState::Working);
        assert!(moving(&working, 10 * minute));
        // A reviewed answer on its way: a minute from the review.
        let mut sending = exchange("s", Incoming, ExchangeState::AwaitingReview);
        assert!(!moving(&sending, 0), "still the owner's to decide");
        sending.review = Some(cypher_proto::ReviewDecision {
            send: true,
            text: None,
            at: 5 * minute,
        });
        assert!(moving(&sending, 5 * minute + 1) && !moving(&sending, 6 * minute));
        // Settled questions never move.
        assert!(!moving(
            &exchange("a", Outgoing, ExchangeState::Answered),
            0
        ));
    }

    #[test]
    fn a_connection_names_the_person_then_their_workspace() {
        let mut peer = conn("c", None, 0).peer;
        assert_eq!(peer_names(&peer), ("Alice".into(), Some("Ada".into())));
        peer.owner_name = "  ".into();
        assert_eq!(peer_names(&peer), ("Ada".into(), None));
        assert_eq!(first_name("Dana Kim"), "Dana");
        assert_eq!(first_name("Dana"), "Dana");
    }

    #[test]
    fn what_a_connection_reads_is_said_in_a_few_words() {
        let projects: Vec<(String, String)> = ["billing", "web-api", "mobile-app", "docs"]
            .iter()
            .map(|name| (name.to_string(), name.to_string()))
            .collect();
        let only = |ids: &[&str]| {
            narrowed(
                &BotScope {
                    projects: ProjectGrant::All,
                    ..Default::default()
                },
                ids.iter().map(|id| id.to_string()).collect(),
            )
        };
        let words = |scope: Option<&BotScope>| reads_words(scope, &projects, "Atlas");
        assert_eq!(words(None), "everything Atlas reads");
        assert_eq!(words(Some(&only(&[]))), "no projects");
        assert_eq!(words(Some(&only(&["billing"]))), "billing");
        assert_eq!(
            words(Some(&only(&["web-api", "billing"]))),
            "billing, web-api"
        );
        assert_eq!(
            words(Some(&only(&["billing", "web-api", "docs"]))),
            "billing, web-api +1"
        );
        // Every project ticked reads as the whole.
        assert_eq!(
            words(Some(&only(&["billing", "web-api", "mobile-app", "docs"]))),
            "everything Atlas reads"
        );
    }

    #[test]
    fn an_opened_question_without_an_answer_says_why() {
        use ExchangeDirection::{Incoming, Outgoing};
        let words = |e: &BotExchange| unanswered_words(e, "Atlas");
        assert_eq!(
            words(&exchange("a", Incoming, ExchangeState::Working)),
            "Atlas is writing its answer…"
        );
        assert_eq!(
            words(&exchange("a", Incoming, ExchangeState::Declined)),
            "Declined."
        );
        let mut sending = exchange("a", Incoming, ExchangeState::AwaitingReview);
        sending.review = Some(cypher_proto::ReviewDecision {
            send: true,
            text: None,
            at: 1,
        });
        assert_eq!(words(&sending), "Sending your answer…");
        assert_eq!(
            words(&exchange("a", Outgoing, ExchangeState::Queued)),
            "Waiting for their answer."
        );
        assert_eq!(
            words(&exchange("a", Outgoing, ExchangeState::Expired)),
            "No answer came; it expired."
        );
    }

    #[test]
    fn an_open_invite_says_how_long_it_has_left() {
        let hour = 3_600_000;
        assert_eq!(left_words(5 * 24 * hour + 1, 0), "5d left");
        assert_eq!(left_words(3 * hour, 0), "3h left");
        assert_eq!(left_words(10 * 60_000, 0), "soon");
        assert_eq!(left_words(0, hour), "soon");
    }

    #[test]
    fn live_connections_and_open_invites_only() {
        let mut revoked = conn("revoked", Some(99), 0);
        revoked.revoked_at = Some(5);
        let connections = vec![conn("quiet", None, 10), revoked, conn("busy", Some(50), 1)];
        let ids: Vec<&str> = live_connections(&connections, "b")
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["busy", "quiet"]);
        let mut used = invite("used", 3, 100);
        used.uses = 1;
        let invites = vec![
            invite("old", 1, 100),
            used,
            invite("new", 2, 100),
            invite("gone", 4, 10),
        ];
        let ids: Vec<&str> = open_invites(&invites, "b", 50)
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, ["new", "old"]);
    }

    #[test]
    fn narrowing_keeps_the_bots_reading_depth() {
        let bot = BotScope {
            projects: ProjectGrant::All,
            transcripts: false,
            include_sessions: vec!["s".into()],
            ..Default::default()
        };
        let scope = narrowed(&bot, vec!["a".into()]);
        assert_eq!(
            scope.projects,
            ProjectGrant::Selected {
                space_ids: vec!["a".into()]
            }
        );
        assert!(!scope.transcripts);
        assert!(scope.include_sessions.is_empty());
    }
}
