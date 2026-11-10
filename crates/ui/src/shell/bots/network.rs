//! The **Network** card (§13.1): connections with their reply mode and last
//! exchange, pending invites, and the connection and invite panels.
//!
//! Bots connect through the owner's account (§11), so a signed-out device
//! shows a quiet "Sign in" state in place of the controls. Invites are
//! RPCs on the bot's host; an engine that does not have them yet answers
//! an unknown method, and the panel says so instead of failing.

use super::panel::{PanelHead, PanelItem};
use super::parts::{self, CallError};
use super::*;
use crate::kit::theme::MonoStyled as _;
use cypher_proto::{
    BotConnection, BotExchange, BotInvite, ExchangeDirection, ExchangeState, PeerBot, ReplyMode,
};

/// The lead column of network rows (peer avatars, the invite and question
/// glyphs): with its 6px inset and 8px gap the titles line up with the
/// other cards' rows.
const LEAD: f32 = 18.0;

/// Exchanges a list shows.
const RECENT_EXCHANGES: usize = 8;

/// "Auto replies · last question 2h ago", "Review replies · no questions
/// yet", "Revoking…".
pub(super) fn connection_sub(conn: &BotConnection, last: Option<&str>) -> String {
    if conn.revoke_requested_at.is_some() {
        return "Revoking…".into();
    }
    let mode = mode_words(conn.reply_mode);
    match last {
        Some(last) => format!("{mode} · {last}"),
        None => format!("{mode} · no questions yet"),
    }
}

fn mode_words(mode: ReplyMode) -> &'static str {
    match mode {
        ReplyMode::Auto => "Auto replies",
        ReplyMode::Review => "Review replies",
        ReplyMode::Off => "Replies off",
    }
}

pub(super) fn exchange_state_words(state: ExchangeState) -> &'static str {
    match state {
        ExchangeState::Queued => "queued",
        ExchangeState::Delivered => "delivered",
        ExchangeState::Working => "working",
        ExchangeState::AwaitingReview => "awaiting your review",
        ExchangeState::Answered => "answered",
        ExchangeState::Declined => "declined",
        ExchangeState::Expired => "expired",
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
                        let item = serde_json::from_value::<BotConnection>(value)
                            .map(|c| PanelItem::Connection(c.id))
                            .unwrap_or(PanelItem::Network);
                        this.show_panel(item, cx);
                    }
                    Err(err) => {
                        this.board.network.redeem_error = Some(this.network_failure(err, cx))
                    }
                }
            },
        );
    }

    /// The sidebar card: only its header. It says how many workspaces are
    /// connected, or, in the amber of Needs you, how many answers wait on
    /// the owner's review; those rows live in Needs you, so a click opens
    /// the one review (or Needs you for several), else the Network panel.
    pub(super) fn render_network_card(
        &self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = cypher_proto::now_ms();
        let (connections, invites, reviews) = {
            let state = self.state.read(cx);
            let records = &state.bot_records;
            let reviews: Vec<String> = records
                .exchanges
                .iter()
                .filter(|e| e.bot_id == bot.id && e.awaiting_review())
                .map(|e| e.id.clone())
                .collect();
            (
                live_connections(&records.connections, &bot.id).len(),
                open_invites(&records.invites, &bot.id, now).len(),
                reviews,
            )
        };
        let (hint, color, open) = match reviews.as_slice() {
            [] => {
                let hint = if !self.network_ready(cx) {
                    "Sign in to connect".to_string()
                } else {
                    let mut parts = vec![match connections {
                        0 => "No connections".to_string(),
                        1 => "1 connection".to_string(),
                        n => format!("{n} connections"),
                    }];
                    if invites > 0 {
                        parts.push(format!(
                            "{invites} open invite{}",
                            if invites == 1 { "" } else { "s" }
                        ));
                    }
                    parts.join(" · ")
                };
                (hint, None, PanelItem::Network)
            }
            [one] => (
                "1 to review".to_string(),
                Some(theme.warning),
                PanelItem::Review(one.clone()),
            ),
            many => (
                format!("{} to review", many.len()),
                Some(theme.warning),
                PanelItem::NeedsYou,
            ),
        };
        Some(parts::header_card(
            "bot-network-card",
            "Network",
            hint,
            color,
            open,
            self.card_focused("network", cx),
            theme,
            cx,
        ))
    }

    /// A connection row: the peer's neutral avatar, "<bot> · <owner>", and
    /// its reply mode and last exchange.
    fn connection_row(
        &self,
        conn: &BotConnection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let key = format!("bot-panel-connection-{}", conn.id);
        let item = PanelItem::Connection(conn.id.clone());
        let last = conn.last_exchange_at.map(parts::ago_ms);
        let revoking = conn.revoke_requested_at.is_some();
        div()
            .id(SharedString::from(key.clone()))
            .pl(px(6.0))
            .pr(px(8.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                &key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.05),
            ))
            .on_hover(motion::hover_listener(key.clone()))
            .on_click(cx.listener(move |this, _, _, cx| this.open_panel(item.clone(), cx)))
            .when(revoking, |el| el.opacity(0.6))
            .child(peer_avatar(&conn.peer, LEAD, theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .gap(px(4.0))
                            .text_size(px(13.0))
                            .line_height(px(17.0))
                            .child(div().flex_none().text_color(theme.text).child(
                                SharedString::from(transcript::single_line(&conn.peer.name)),
                            ))
                            .when(!conn.peer.owner_name.is_empty(), |el| {
                                el.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme.text_muted.opacity(0.85))
                                        .child(SharedString::from(format!(
                                            "· {}",
                                            conn.peer.owner_name
                                        ))),
                                )
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .line_height(px(14.0))
                            .text_color(if conn.reply_mode == ReplyMode::Off && !revoking {
                                theme.text_faint
                            } else {
                                theme.text_muted.opacity(0.85)
                            })
                            .child(SharedString::from(connection_sub(conn, last.as_deref()))),
                    ),
            )
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_network_panel(
        &mut self,
        bot: &Bot,
        item: &PanelItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        // The code is shown once: leaving the invite panel drops it.
        if *item != PanelItem::Invite && self.board.network.issued.is_some() {
            self.board.network.issued = None;
            self.board.network.invite_error = None;
        }
        match item {
            PanelItem::Network => Some(self.render_network_list(bot, theme, cx)),
            PanelItem::Invite => Some(self.render_invite_panel(bot, theme, cx)),
            PanelItem::Connection(id) => self.render_connection(bot, id, theme, cx),
            _ => None,
        }
    }

    fn render_network_list(
        &mut self,
        bot: &Bot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (PanelHead, gpui::Div) {
        let now = cypher_proto::now_ms();
        let ready = self.network_ready(cx);
        let (connections, invites, exchanges) = {
            let state = self.state.read(cx);
            let records = &state.bot_records;
            let mut exchanges: Vec<BotExchange> = records
                .exchanges
                .iter()
                .filter(|e| e.bot_id == bot.id)
                .cloned()
                .collect();
            exchanges.sort_by_key(|e| std::cmp::Reverse(e.asked_at));
            exchanges.truncate(RECENT_EXCHANGES);
            (
                live_connections(&records.connections, &bot.id)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                open_invites(&records.invites, &bot.id, now)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                exchanges,
            )
        };
        let head = PanelHead {
            glyph: icons::GLOBAL,
            tint: None,
            title: "Network".into(),
        };
        // Nothing yet: one centred message, with the way in.
        if connections.is_empty() && invites.is_empty() && exchanges.is_empty() {
            let empty = if ready {
                parts::panel_empty(
                    theme,
                    icons::GLOBAL,
                    "No connections yet",
                    format!(
                        "Connected workspaces can ask {} questions, read-only, within what \
                         you allow.",
                        bot.name
                    ),
                )
                .child(
                    div()
                        .pt(px(8.0))
                        .flex()
                        .flex_row()
                        .gap(px(4.0))
                        .child(
                            parts::action(
                                "bot-network-invite",
                                icons::PLUS,
                                "Invite a workspace",
                                theme.text,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.create_invite(cx))),
                        )
                        .child(
                            parts::action(
                                "bot-network-redeem",
                                icons::LINK,
                                "Connect with a code",
                                theme.text,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    let input = this.redeem_input(cx);
                                    window.focus(&input.focus_handle(cx), cx);
                                    this.show_panel(PanelItem::Invite, cx);
                                },
                            )),
                        ),
                )
            } else {
                parts::panel_empty(
                    theme,
                    icons::CLOUD_OFF,
                    "Sign in to connect workspaces",
                    "Use the account menu. Connections go through your Cypher account.",
                )
            };
            self.board.panel_empty = Some(empty.into_any_element());
            return (head, div());
        }
        let mut body = div().p(px(8.0)).flex().flex_col();
        body = if ready {
            body.child(
                div()
                    .px(px(4.0))
                    .pt(px(2.0))
                    .flex()
                    .flex_row()
                    .gap(px(4.0))
                    .child(
                        parts::action(
                            "bot-network-invite",
                            icons::PLUS,
                            "Invite a workspace",
                            theme.text,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.create_invite(cx))),
                    )
                    .child(
                        parts::action(
                            "bot-network-redeem",
                            icons::LINK,
                            "Connect with a code",
                            theme.text,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            let input = this.redeem_input(cx);
                            window.focus(&input.focus_handle(cx), cx);
                            this.show_panel(PanelItem::Invite, cx);
                        })),
                    ),
            )
        } else {
            body.child(signed_out(theme))
        };
        let peers: std::collections::HashMap<String, PeerBot> = connections
            .iter()
            .map(|c| (c.id.clone(), c.peer.clone()))
            .collect();
        body = body.child(parts::section_label(theme, "Connections"));
        if connections.is_empty() {
            body = body.child(parts::quiet_line(theme, "None yet."));
        }
        for conn in &connections {
            body = body.child(self.connection_row(conn, theme, cx));
        }
        if !invites.is_empty() {
            body = body.child(parts::section_label(theme, "Open invites"));
            for invite in &invites {
                body = body.child(self.invite_row(invite, ready, theme, cx));
            }
        }
        if !exchanges.is_empty() {
            body = body.child(parts::section_label(theme, "Recent questions"));
            for exchange in &exchanges {
                let peer = peers.get(&exchange.connection_id);
                body = body.child(exchange_row(exchange, peer, &bot.name, theme));
            }
        }
        (head, body)
    }

    fn invite_row(
        &self,
        invite: &BotInvite,
        ready: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let id = invite.id.clone();
        let revoking = invite.revoke_requested_at.is_some();
        invite_glyph_row(&format!("bot-panel-invite-{}", invite.id), theme)
            .py(px(8.0))
            .child(super::board::two_lines(
                format!("Invite code · made {}", parts::ago_ms(invite.created_at)),
                if revoking {
                    "Revoking…".into()
                } else {
                    format!(
                        "Expires {} · {} of {} used",
                        expiry_words(invite.expires_at),
                        invite.uses,
                        invite.max_uses
                    )
                },
                String::new(),
                theme,
            ))
            .when(ready && !revoking, |el| {
                el.child(
                    parts::action(
                        format!("bot-invite-revoke-{}", invite.id),
                        icons::CLOSE,
                        "Revoke",
                        theme.text_muted,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.board_mutate(
                            serde_json::json!({ "op": "revokeBotInvite", "inviteId": id }),
                            cx,
                        );
                    })),
                )
            })
    }

    /// Create an invite, or redeem another owner's code.
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
        self.set_panel_back("bot-invite-back", "Back to Network", PanelItem::Network, cx);
        if !self.network_ready(cx) {
            return (
                head,
                div()
                    .p(px(8.0))
                    .flex()
                    .flex_col()
                    .child(signed_out(theme))
                    .child(privacy_note(theme)),
            );
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
        let body = div()
            .p(px(8.0))
            .flex()
            .flex_col()
            .child(parts::section_label(theme, "Invite a workspace"))
            .child(
                parts::quiet_line(
                    theme,
                    "Send the code to the other person; they connect one of their workspaces with it.",
                )
                .pt(px(0.0)),
            )
            .child(invite_body)
            .child(parts::section_label(theme, "Connect with a code"))
            .child(
                parts::quiet_line(
                    theme,
                    format!(
                        "Got a code from someone? Connect {} to their workspace.",
                        bot.name
                    ),
                )
                .pt(px(0.0)),
            )
            .child(redeem)
            .children(net.redeem_error.clone().map(|(error, quiet)| {
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

    /// One connection: its reply mode, what the peer may read, its
    /// questions, and Revoke.
    fn render_connection(
        &mut self,
        bot: &Bot,
        id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        let ready = self.network_ready(cx);
        let (conn, exchanges, projects) = {
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
            (conn, exchanges, projects)
        };
        let head = PanelHead {
            glyph: icons::LINK,
            tint: None,
            title: conn.peer.name.clone(),
        };
        // Whose workspace it is, beside its name.
        if !conn.peer.owner_name.trim().is_empty() || conn.peer.owner_avatar_url.is_some() {
            self.board.panel_title_note = Some(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .text_size(px(12.5))
                    .text_color(theme.text_muted)
                    .child(peer_avatar(&conn.peer, 16.0, theme))
                    .child(div().min_w_0().truncate().child(SharedString::from(
                        transcript::single_line(&conn.peer.owner_name),
                    )))
                    .into_any_element(),
            );
        }
        self.set_panel_back(
            "bot-connection-back",
            "Back to Network",
            PanelItem::Network,
            cx,
        );
        let revoked = conn.revoked_at.is_some();
        let revoking = conn.revoke_requested_at.is_some();
        let editable = ready && !revoked && !revoking;
        if editable {
            let (revoke_id, name) = (conn.id.clone(), conn.peer.name.clone());
            self.set_panel_actions(vec![
                super::items::icon_button(
                    "bot-conn-revoke",
                    icons::TRASH_BIN_MINIMALISTIC,
                    "Revoke connection",
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
            ]);
        }
        // The owner is beside the title, with their photo.
        let mut body = div().p(px(8.0)).flex().flex_col();
        body = body.child(parts::fact(
            theme,
            "Connected",
            SharedString::from(parts::past_full_ms(conn.created_at)),
        ));
        if revoked || revoking {
            body = body.child(parts::fact(
                theme,
                "Status",
                super::items::outcome_value(
                    if revoked {
                        "Revoked"
                    } else {
                        "Revoking on both sides…"
                    },
                    false,
                    theme,
                ),
            ));
        }

        // ── replies ──
        let mode_detail = match conn.reply_mode {
            ReplyMode::Auto => "Answers go back as soon as they are written.",
            ReplyMode::Review => "Answers wait for you as drafts in Needs you.",
            ReplyMode::Off => "Questions are declined.",
        };
        if editable {
            body = body.child(parts::section_label(theme, "Replies"));
            body = body.child(div().mx(px(8.0)).child(parts::segmented(
                "bot-conn-mode",
                &[
                    (ReplyMode::Auto, "Auto"),
                    (ReplyMode::Review, "Review"),
                    (ReplyMode::Off, "Off"),
                ],
                conn.reply_mode,
                |this, mode, cx| this.update_connection(None, Some(mode), None, cx),
                theme,
                cx,
            )));
            body = body.child(
                div()
                    .px(px(8.0))
                    .pt(px(6.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(mode_detail)),
            );
        } else {
            body = body.child(parts::fact(
                theme,
                "Replies",
                SharedString::from(mode_detail),
            ));
        }

        // ── what the peer's questions may read ──
        body = body.child(parts::section_label(theme, "May read"));
        let narrowed_ids: Option<Vec<String>> = conn.scope.as_ref().map(|s| match &s.projects {
            ProjectGrant::Selected { space_ids } => space_ids.clone(),
            ProjectGrant::All => projects.iter().map(|(id, _)| id.clone()).collect(),
            ProjectGrant::None => Vec::new(),
        });
        let same_label = format!("Same as {}", bot.name);
        let mode = if narrowed_ids.is_some() { 1usize } else { 0 };
        if editable && !projects.is_empty() {
            body = body.child(div().mx(px(8.0)).child(parts::segmented(
                "bot-conn-scope",
                &[
                    (0usize, same_label.as_str()),
                    (1usize, "Only some projects"),
                ],
                mode,
                |this, pick, cx| {
                    let narrow = pick == 1;
                    this.update_connection(Some(narrow), None, None, cx)
                },
                theme,
                cx,
            )));
        }
        match &narrowed_ids {
            None => {
                body = body.child(
                    div()
                        .px(px(8.0))
                        .pt(px(6.0))
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        // A peer only ever reads: what the bot may do stays out.
                        .child(SharedString::from(format!("{same_label}: {}.", {
                            let reading = Bot {
                                capabilities: Default::default(),
                                ..bot.clone()
                            };
                            super::chat::access_summary(&reading, self.state.read(cx))
                        }))),
                );
            }
            Some(ids) => {
                let rows = projects.iter().map(|(space_id, name)| {
                    let on = ids.contains(space_id);
                    let pick = space_id.clone();
                    div()
                        .id(SharedString::from(format!("bot-conn-project-{space_id}")))
                        .h(px(28.0))
                        .px(px(8.0))
                        .rounded(px(7.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(9.0))
                        .when(editable, |el| {
                            el.cursor_pointer()
                                .hover(|s| s.bg(theme.element_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.update_connection(None, None, Some(pick.clone()), cx)
                                }))
                        })
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
                body = body.child(
                    div()
                        .mx(px(4.0))
                        .mt(px(6.0))
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .children(rows),
                );
            }
        }
        body = body.child(parts::section_label(theme, "Recent questions"));
        if exchanges.is_empty() {
            body = body.child(parts::quiet_line(theme, "No questions yet."));
        }
        for exchange in &exchanges {
            body = body.child(exchange_row(exchange, Some(&conn.peer), &bot.name, theme));
        }
        Some((head, body))
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

/// A row led by the invite glyph.
fn invite_glyph_row(key: &str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(key.to_string()))
        .pl(px(6.0))
        .pr(px(8.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .size(px(LEAD))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    icon(icons::LINK)
                        .size(px(14.0))
                        .text_color(theme.text_muted),
                ),
        )
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

/// One question between bots: the other person, who asked whom, the
/// question, its state. A former connection's shows its direction instead.
fn exchange_row(
    exchange: &BotExchange,
    peer: Option<&PeerBot>,
    bot: &str,
    theme: &Theme,
) -> gpui::Div {
    let incoming = exchange.direction == ExchangeDirection::Incoming;
    let who = match (incoming, peer) {
        (true, Some(peer)) => format!("{} asked {bot}", peer.name),
        (false, Some(peer)) => format!("{bot} asked {}", peer.name),
        (true, None) => format!("A former connection asked {bot}"),
        (false, None) => format!("{bot} asked a former connection"),
    };
    let lead = match peer {
        Some(peer) => peer_avatar(peer, 16.0, theme),
        None => icon(if incoming {
            icons::ARROW_DOWN
        } else {
            icons::ARROW_UP
        })
        .size(px(12.0))
        .text_color(theme.text_muted)
        .into_any_element(),
    };
    let state_color = match exchange.state {
        ExchangeState::AwaitingReview => theme.warning,
        ExchangeState::Declined | ExchangeState::Expired => theme.text_faint,
        _ => theme.text_muted.opacity(0.85),
    };
    div()
        .pl(px(6.0))
        .pr(px(8.0))
        .py(px(6.0))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.0))
        .child(
            div()
                .w(px(LEAD))
                .flex_none()
                .flex()
                .justify_center()
                .pt(px(1.0))
                .child(lead),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .line_height(px(17.0))
                        .text_color(theme.text)
                        .child(SharedString::from(transcript::single_line(
                            &exchange.question,
                        ))),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(4.0))
                        .text_size(px(11.0))
                        .line_height(px(14.0))
                        .text_color(theme.text_muted.opacity(0.85))
                        .child(SharedString::from(who))
                        .child("·")
                        .child(
                            div()
                                .text_color(state_color)
                                .child(SharedString::from(exchange_state_words(exchange.state))),
                        )
                        .child("·")
                        .child(SharedString::from(parts::ago_ms(exchange.asked_at))),
                ),
        )
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
             delivered. Answers are scanned for secrets before they leave; that is a \
             safety net, not a guarantee.",
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

    #[test]
    fn connection_lines_read_mode_then_last_question() {
        let mut c = conn("c", None, 0);
        assert_eq!(connection_sub(&c, None), "Auto replies · no questions yet");
        c.reply_mode = ReplyMode::Review;
        assert_eq!(
            connection_sub(&c, Some("last 2h")),
            "Review replies · last 2h"
        );
        c.revoke_requested_at = Some(1);
        assert_eq!(connection_sub(&c, Some("last 2h")), "Revoking…");
        assert_eq!(
            exchange_state_words(ExchangeState::AwaitingReview),
            "awaiting your review"
        );
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
