//! `list_activity`: the workspace's own activity log (§4.4) as its agent
//! reads it, for "what did you do while I was away". The owner's threads
//! and routines read it; peer threads never do (it names the owner's work).

use chrono::{DateTime, Utc};
use serde::Deserialize;

use cypher_proto::{self as p, ActivityGroup, BotActivity, BotChat, BotOrigin, BotThread, Chat};

use super::Bots;
use super::tools::when;
use crate::EngineError;

/// Entries one answer lists at most.
const LIST_MAX: usize = 60;
const DEFAULT_HOURS: f64 = 24.0;
/// The log keeps the newest [`p::KEEP_ACTIVITY`] entries, about two weeks.
const MAX_HOURS: f64 = 336.0;

/// The tools of this track a thread may call.
pub(super) fn activity_tools(marker: &BotChat) -> Vec<&'static str> {
    match (marker.origin, &marker.thread) {
        (BotOrigin::Owner, BotThread::Main | BotThread::Work { .. })
        | (BotOrigin::Routine, BotThread::Routine { .. }) => p::BOT_ACTIVITY_TOOLS.to_vec(),
        _ => Vec::new(),
    }
}

/// This track's section of the thread's prompt.
pub(super) fn activity_prompt(marker: &BotChat) -> Option<String> {
    (!activity_tools(marker).is_empty()).then(|| {
        "Your activity:\n- list_activity reads what you did yourself: plans proposed and \
         decided, sessions you started or stopped, routines run, memory you changed, questions \
         to and from other workspaces. Use it for \"what did you do while I was away\"; \
         session_activity is for what happened in the sessions."
            .to_string()
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ListActivity {
    since_hours: Option<f64>,
    kind: Option<String>,
}

fn group_word(group: ActivityGroup) -> &'static str {
    match group {
        ActivityGroup::Plans => "plans",
        ActivityGroup::Sessions => "sessions",
        ActivityGroup::Routines => "routines",
        ActivityGroup::Notes => "memory",
        ActivityGroup::Network => "network",
    }
}

/// The answer: `bot_id`'s entries since `since`, newest first, in `kind`'s
/// group when given.
pub(super) fn render_activity(
    entries: &[BotActivity],
    bot_id: &str,
    since: DateTime<Utc>,
    kind: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String, EngineError> {
    let kind = kind.map(str::trim).filter(|k| !k.is_empty());
    if let Some(kind) = kind
        && !["plans", "sessions", "routines", "memory", "network"].contains(&kind)
    {
        return Err(EngineError::Other(format!(
            "Unknown kind \"{kind}\": use plans, sessions, routines, memory or network."
        )));
    }
    let mut mine: Vec<&BotActivity> = entries
        .iter()
        .filter(|a| a.bot_id == bot_id && a.at >= since.timestamp_millis())
        .filter(|a| kind.is_none_or(|k| group_word(a.kind.group()) == k))
        .collect();
    mine.sort_by(|a, b| b.at.cmp(&a.at).then(b.id.cmp(&a.id)));
    let hours = ((now - since).num_minutes() as f64 / 60.0).round();
    if mine.is_empty() {
        return Ok(format!("Nothing in your activity in the last {hours} h."));
    }
    let mut out = vec![format!(
        "Your activity in the last {hours} h, newest first ({}):",
        mine.len()
    )];
    for entry in mine.iter().take(LIST_MAX) {
        let at = DateTime::from_timestamp_millis(entry.at).unwrap_or(now);
        let mut line = format!(
            "- {} · {} · {}",
            when(at, now),
            group_word(entry.kind.group()),
            entry.text.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        if let Some(chat) = &entry.chat_id {
            line.push_str(&format!(" (session {chat})"));
        }
        out.push(line);
    }
    if mine.len() > LIST_MAX {
        out.push(format!(
            "{} older entries not shown; narrow with since_hours or kind.",
            mine.len() - LIST_MAX
        ));
    }
    Ok(out.join("\n"))
}

impl Bots {
    /// Answer `tool` when it is this track's; `None` otherwise.
    pub(super) fn activity_tool(
        &self,
        chat: &Chat,
        marker: &BotChat,
        tool: &str,
        raw: &serde_json::Value,
    ) -> Option<Result<String, EngineError>> {
        if !p::BOT_ACTIVITY_TOOLS.contains(&tool) {
            return None;
        }
        Some(self.list_activity(chat, marker, raw))
    }

    fn list_activity(
        &self,
        chat: &Chat,
        marker: &BotChat,
        raw: &serde_json::Value,
    ) -> Result<String, EngineError> {
        if activity_tools(marker).is_empty() {
            return Err(EngineError::Other(
                "list_activity isn't available in this session.".into(),
            ));
        }
        let workspace = &self.inner.workspace;
        let bot = workspace
            .bot(&marker.bot_id)?
            .filter(|bot| bot.chat_id == chat.id || !marker.is_main())
            .ok_or_else(|| EngineError::Other("This workspace has been deleted.".into()))?;
        let args: ListActivity = serde_json::from_value(raw.clone())
            .map_err(|err| EngineError::Other(format!("Invalid arguments: {err}")))?;
        let hours = args
            .since_hours
            .unwrap_or(DEFAULT_HOURS)
            .clamp(0.25, MAX_HOURS);
        let now = Utc::now();
        let since = now - chrono::Duration::minutes((hours * 60.0) as i64);
        render_activity(
            &workspace.read_bot_records().activity,
            &bot.id,
            since,
            args.kind.as_deref(),
            now,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::ActivityKind;

    fn entry(
        id: &str,
        bot: &str,
        kind: ActivityKind,
        minutes_ago: i64,
        now: DateTime<Utc>,
    ) -> BotActivity {
        BotActivity {
            id: id.into(),
            bot_id: bot.into(),
            at: (now - chrono::Duration::minutes(minutes_ago)).timestamp_millis(),
            kind,
            text: format!("entry {id}"),
            chat_id: None,
            work_id: None,
            space_id: None,
        }
    }

    #[test]
    fn activity_lists_this_workspaces_window_newest_first() {
        let now = Utc::now();
        let entries = [
            entry("old", "b1", ActivityKind::RoutineRan, 60 * 30, now),
            entry("ran", "b1", ActivityKind::RoutineRan, 90, now),
            entry("plan", "b1", ActivityKind::PlanApproved, 10, now),
            entry("other", "b2", ActivityKind::PlanApproved, 5, now),
        ];
        let day = now - chrono::Duration::hours(24);
        let text = render_activity(&entries, "b1", day, None, now).unwrap();
        assert!(
            text.starts_with("Your activity in the last 24 h, newest first (2):"),
            "{text}"
        );
        let plan = text.find("plans · entry plan").unwrap();
        let ran = text.find("routines · entry ran").unwrap();
        assert!(plan < ran);
        assert!(!text.contains("entry old") && !text.contains("entry other"));
        let only = render_activity(&entries, "b1", day, Some("routines"), now).unwrap();
        assert!(only.contains("entry ran") && !only.contains("entry plan"));
        assert!(render_activity(&entries, "b1", day, Some("cats"), now).is_err());
        let hour = now - chrono::Duration::minutes(1);
        assert!(
            render_activity(&entries, "b1", hour, None, now)
                .unwrap()
                .starts_with("Nothing in your activity")
        );
    }

    #[test]
    fn peers_never_read_the_log() {
        let peer = BotChat::thread(
            "b1",
            BotOrigin::Peer,
            BotThread::Peer {
                connection_id: "c".into(),
            },
        );
        assert!(activity_tools(&peer).is_empty());
        assert_eq!(activity_tools(&BotChat::owner("b1")), ["list_activity"]);
    }
}
