//! Agent bots: creation on the host and the bot tool surface.

use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BotToolParams {
    chat_id: String,
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
    /// The Pi tool call this is (the plan card is drawn at it).
    #[serde(default)]
    tool_call_id: Option<String>,
}

pub(super) fn create_bot(rpc: &EngineRpc, params: serde_json::Value) -> Result<RpcReply, RpcError> {
    let request: crate::bots::CreateBot = parse_params(strip_target(params))?;
    let bot = rpc.bots()?.create(request).map_err(failed)?;
    RpcReply::value(&bot)
}

/// `{text}`, or `{text, isError: true}` for a refusal or failure: still an
/// answer the model should read, so it comes back as text, flagged so the
/// bot tools extension returns it as an error result (the chat draws a
/// failed chip, not the plan card a refused `propose_plan` would get). Only
/// a malformed call is an RPC error.
pub(super) async fn bot_tool(
    rpc: &EngineRpc,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let p: BotToolParams = parse_params(params)?;
    let mut args = if p.args.is_null() {
        serde_json::json!({})
    } else {
        p.args
    };
    // Set from the call itself, over anything the model put in its args.
    if let Some(object) = args.as_object_mut() {
        object.remove(crate::bots::TOOL_CALL_ID_ARG);
        if let Some(id) = p.tool_call_id.filter(|id| !id.is_empty()) {
            object.insert(crate::bots::TOOL_CALL_ID_ARG.into(), id.into());
        }
    }
    match rpc.bots()?.tool(&p.chat_id, &p.tool, args).await {
        Ok(text) => RpcReply::value(&serde_json::json!({ "text": text })),
        Err(err) => {
            RpcReply::value(&serde_json::json!({ "text": err.to_string(), "isError": true }))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBotInviteParams {
    bot_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RedeemBotInviteParams {
    bot_id: String,
    code: String,
}

/// `CreateBotInvite` on the bot's host: `{inviteId, code, expiresAt}`.
pub(super) async fn create_bot_invite(
    rpc: &EngineRpc,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let p: CreateBotInviteParams = parse_params(strip_target(params))?;
    let invite = rpc.bots()?.create_invite(&p.bot_id).await.map_err(failed)?;
    RpcReply::value(&invite)
}

/// `RedeemBotInvite` on the bot's host: the new `BotConnection`.
pub(super) async fn redeem_bot_invite(
    rpc: &EngineRpc,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let p: RedeemBotInviteParams = parse_params(strip_target(params))?;
    let connection = rpc
        .bots()?
        .redeem_invite(&p.bot_id, &p.code)
        .await
        .map_err(failed)?;
    RpcReply::value(&connection)
}

impl EngineRpc {
    /// The owner's edits of bot records: each writes only the fields an
    /// owner device owns (docs/design/agent-bots.md §13.1); the bot's host
    /// watches the rows and acts on them.
    pub(super) fn mutate_bot_record(&self, params: MutateParams) -> Result<(), RpcError> {
        use crate::host::workspace_host::Fields;
        use cypher_proto as p;
        let ws = &self.workspace;
        let now = p::now_ms();
        let fields =
            |pairs: &[(&str, serde_json::Value)]| -> serde_json::Map<String, serde_json::Value> {
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect()
            };
        let missing = |what: &str| RpcError::Failed(format!("No such {what}."));
        let decision = |approve: bool| serde_json::json!(p::PlanDecision { approve, at: now });
        match params {
            MutateParams::ResolveBotPlan { work_id, approve } => {
                let work: p::BotWork = ws
                    .record(p::KIND_BOT_WORK, &work_id)
                    .ok_or_else(|| missing("plan"))?;
                if !work.awaiting_approval() {
                    return Err(RpcError::Failed("This plan was already decided.".into()));
                }
                ws.update_record(
                    p::KIND_BOT_WORK,
                    &work_id,
                    fields(&[("decision", decision(approve))]),
                )
                .map_err(failed)?;
            }
            MutateParams::StopBotWork { work_id } => {
                if !ws
                    .update_record(
                        p::KIND_BOT_WORK,
                        &work_id,
                        fields(&[("stopRequestedAt", now.into())]),
                    )
                    .map_err(failed)?
                {
                    return Err(missing("task"));
                }
            }
            MutateParams::AddBotWorkTime { work_id, minutes } => {
                let work = ws
                    .record::<p::BotWork>(p::KIND_BOT_WORK, &work_id)
                    .ok_or_else(|| missing("task"))?;
                if work.state != p::WorkState::Running {
                    return Err(RpcError::BadParams(
                        "Only a running plan can be given more time.".into(),
                    ));
                }
                let added = work.time_added_minutes.unwrap_or(0);
                if minutes == 0 || added + minutes > p::PLAN_TIME_ADDED_MAX_MINUTES {
                    return Err(RpcError::BadParams(format!(
                        "A plan can be given at most {} more in all.",
                        p::describe_minutes(p::PLAN_TIME_ADDED_MAX_MINUTES)
                    )));
                }
                ws.update_record(
                    p::KIND_BOT_WORK,
                    &work_id,
                    fields(&[("timeAddedMinutes", (added + minutes).into())]),
                )
                .map_err(failed)?;
            }
            MutateParams::DeleteBotWork { work_id } => {
                let work = ws
                    .record::<p::BotWork>(p::KIND_BOT_WORK, &work_id)
                    .ok_or_else(|| missing("task"))?;
                if work.is_live() {
                    return Err(RpcError::BadParams(
                        "Stop or cancel this work before deleting it.".into(),
                    ));
                }
                ws.delete_records(p::KIND_BOT_WORK, &[work_id]);
                // Its background session goes with it; the sessions it
                // started in projects are ordinary sessions and stay.
                if let Some(thread) = &work.thread_chat_id
                    && let Err(err) = ws.delete_chat(thread)
                {
                    tracing::warn!(chat = %thread, error = %err, "work session not deleted");
                }
            }
            MutateParams::ResolveBotApproval {
                approval_id,
                approve,
            } => {
                if !ws
                    .update_record(
                        p::KIND_BOT_APPROVALS,
                        &approval_id,
                        fields(&[("decision", decision(approve))]),
                    )
                    .map_err(failed)?
                {
                    return Err(missing("approval"));
                }
            }
            MutateParams::SaveBotRoutine { mut routine } => {
                // Before the clamp: a prompt over its cap is refused, not cut.
                routine.validate().map_err(RpcError::BadParams)?;
                routine.clamp();
                if let Some(space_id) = routine.target.space_id() {
                    let bot = ws
                        .bot(&routine.bot_id)
                        .map_err(failed)?
                        .ok_or_else(|| missing("workspace"))?;
                    if !bot.capabilities.coordinate {
                        return Err(RpcError::BadParams(
                            "Running a routine in a project needs the workspace's Coordinate \
                             permission."
                                .into(),
                        ));
                    }
                    if !bot.scope.sees_space(space_id) {
                        return Err(RpcError::BadParams(
                            "The workspace has no access to that project.".into(),
                        ));
                    }
                }
                routine.updated_at = now;
                ws.put_record(
                    p::KIND_BOT_ROUTINES,
                    &routine.id.clone(),
                    &routine,
                    Fields::ExceptClearing {
                        drop: &[p::BotRoutine::HOST_FIELDS, &["runRequestedAt", "runNote"]]
                            .concat(),
                        clear: p::BotRoutine::OWNER_OPTIONAL_FIELDS,
                    },
                )
                .map_err(failed)?;
            }
            MutateParams::DeleteBotRoutine { routine_id } => {
                // Its sessions go with it; a worktree it made stays on disk.
                let sessions = ws
                    .record::<p::BotRoutine>(p::KIND_BOT_ROUTINES, &routine_id)
                    .map(|r| [r.thread_chat_id, r.manual_chat_id])
                    .unwrap_or_default();
                ws.delete_records(p::KIND_BOT_ROUTINES, &[routine_id]);
                let sessions: Vec<String> = sessions.into_iter().flatten().collect();
                for chat in &sessions {
                    if let Err(err) = ws.delete_chat(chat) {
                        tracing::warn!(chat = %chat, error = %err, "routine session not deleted");
                    }
                }
                // A run still going claims its deleted row back with its
                // next write: stop it, then take the row again.
                let engine = self.sessions.clone();
                let workspace = ws.clone();
                tokio::spawn(async move {
                    for chat in sessions {
                        if matches!(engine.interrupt(&chat).await, Ok(true))
                            && let Err(err) = workspace.delete_chat(&chat)
                        {
                            tracing::warn!(chat = %chat, error = %err, "routine session not deleted");
                        }
                    }
                });
            }
            MutateParams::DeleteBotShortcut { shortcut_id } => {
                ws.delete_records(p::KIND_BOT_SHORTCUTS, &[shortcut_id]);
            }
            MutateParams::OrderBotShortcuts {
                bot_id,
                shortcut_ids,
            } => {
                for (order, id) in shortcut_ids.iter().enumerate() {
                    let Some(shortcut) = ws.record::<p::BotShortcut>(p::KIND_BOT_SHORTCUTS, id)
                    else {
                        continue;
                    };
                    if shortcut.bot_id != bot_id || shortcut.order == order as i64 {
                        continue;
                    }
                    ws.update_record(
                        p::KIND_BOT_SHORTCUTS,
                        id,
                        fields(&[("order", (order as i64).into())]),
                    )
                    .map_err(failed)?;
                }
            }
            MutateParams::RunBotRoutine { routine_id, note } => {
                let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
                if note
                    .as_deref()
                    .is_some_and(|n| n.chars().count() > p::RUN_NOTE_MAX)
                {
                    return Err(RpcError::BadParams(format!(
                        "Keep the note under {} characters.",
                        p::RUN_NOTE_MAX
                    )));
                }
                let mut set = vec![("runRequestedAt", now.into())];
                if let Some(text) = note {
                    set.push(("runNote", serde_json::json!({ "at": now, "text": text })));
                }
                if !ws
                    .update_record(p::KIND_BOT_ROUTINES, &routine_id, fields(&set))
                    .map_err(failed)?
                {
                    return Err(missing("routine"));
                }
            }
            MutateParams::UpdateBotConnection {
                connection_id,
                reply_mode,
                scope,
            } => {
                if let Some(scope) = &scope {
                    scope.validate().map_err(RpcError::BadParams)?;
                }
                let scope = match scope {
                    Some(scope) => {
                        serde_json::to_value(scope).map_err(|e| RpcError::Failed(e.to_string()))?
                    }
                    None => serde_json::Value::Null,
                };
                if !ws
                    .update_record(
                        p::KIND_BOT_CONNECTIONS,
                        &connection_id,
                        fields(&[
                            ("replyMode", serde_json::json!(reply_mode)),
                            ("scope", scope),
                        ]),
                    )
                    .map_err(failed)?
                {
                    return Err(missing("connection"));
                }
            }
            MutateParams::RevokeBotConnection { connection_id } => {
                if !ws
                    .update_record(
                        p::KIND_BOT_CONNECTIONS,
                        &connection_id,
                        fields(&[("revokeRequestedAt", now.into())]),
                    )
                    .map_err(failed)?
                {
                    return Err(missing("connection"));
                }
            }
            MutateParams::RevokeBotInvite { invite_id } => {
                if !ws
                    .update_record(
                        p::KIND_BOT_INVITES,
                        &invite_id,
                        fields(&[("revokeRequestedAt", now.into())]),
                    )
                    .map_err(failed)?
                {
                    return Err(missing("invite"));
                }
            }
            MutateParams::ReviewBotExchange {
                exchange_id,
                send,
                text,
            } => {
                // Refused over its cap rather than cut before it is sent.
                if let Some(text) = &text {
                    p::check_review_text(text).map_err(RpcError::BadParams)?;
                }
                let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                let review = p::ReviewDecision {
                    send,
                    text,
                    at: now,
                };
                if !ws
                    .update_record(
                        p::KIND_BOT_EXCHANGES,
                        &exchange_id,
                        fields(&[("review", serde_json::json!(review))]),
                    )
                    .map_err(failed)?
                {
                    return Err(missing("question"));
                }
            }
            _ => unreachable!("only bot record edits are routed here"),
        }
        Ok(())
    }
}
