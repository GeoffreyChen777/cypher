//! The system prompt appended to a bot's Pi run.
//!
//! It describes the scope in plain words so the model knows what it can be
//! asked about. It is a description only: the tools enforce the scope on
//! every call, and a scope edit applies to the next call even while an older
//! prompt is still in a running session.

use cypher_proto::{Bot, Device, ProjectGrant, Space};

/// Projects named in the prompt before it says "and N more".
const NAMED_PROJECTS: usize = 30;

pub(super) fn system_prompt(bot: &Bot, spaces: &[Space], devices: &[Device]) -> String {
    let scope = &bot.scope;
    let device_name = |id: &str| {
        devices
            .iter()
            .find(|d| d.id == id)
            .map_or("an unknown device", |d| d.name.as_str())
            .to_string()
    };
    let projects = match &scope.projects {
        ProjectGrant::None => "no projects".to_string(),
        ProjectGrant::All => "every project, including ones created later".to_string(),
        ProjectGrant::Selected { space_ids } => {
            let named: Vec<String> = space_ids
                .iter()
                .filter_map(|id| spaces.iter().find(|s| &s.id == id))
                .map(|s| format!("{} (on {})", space_name(s), device_name(&s.device_id)))
                .collect();
            if named.is_empty() {
                "no projects".to_string()
            } else if named.len() > NAMED_PROJECTS {
                format!(
                    "{}, and {} more",
                    named[..NAMED_PROJECTS].join(", "),
                    named.len() - NAMED_PROJECTS
                )
            } else {
                named.join(", ")
            }
        }
    };
    let mut lines = vec![
        format!(
            "You are \"{}\", an agent bot in Cypher. You answer questions about your owner's \
             work: the coding sessions they run with AI agents across their devices, and the \
             projects those sessions belong to.",
            bot.name
        ),
        String::new(),
        "What you can see:".to_string(),
        format!("- Projects: {projects}."),
    ];
    if !scope.include_sessions.is_empty() {
        lines.push(format!(
            "- {} individually shared session(s) outside those projects.",
            scope.include_sessions.len()
        ));
    }
    if !scope.exclude_sessions.is_empty() {
        lines.push(format!(
            "- {} session(s) in those projects are hidden from you.",
            scope.exclude_sessions.len()
        ));
    }
    lines.push(format!(
        "- Archived sessions: {}.",
        if scope.include_archived {
            "included"
        } else {
            "not included"
        }
    ));
    lines.push(if scope.transcripts {
        "- Message text: yes.".to_string()
    } else {
        "- Message text: no. You see titles, the latest-message previews and status only."
            .to_string()
    });
    lines.extend([
        String::new(),
        "How to work:".to_string(),
        "- Use list_projects, list_sessions, search_sessions and read_session to find answers. \
         Never guess about work you have not read."
            .to_string(),
        "- Those tools are all you have: you cannot run commands, read files or change \
         anything."
            .to_string(),
        "- Cite every session an answer draws on as a markdown link to it: \
         [its title](cypher://session/<session id>). The app opens these."
            .to_string(),
        "- When something is outside what you can see, say so plainly instead of speculating."
            .to_string(),
        "- Tool output shows times in UTC.".to_string(),
    ]);
    if let Some(instructions) = bot.instructions.as_deref().filter(|s| !s.is_empty()) {
        lines.extend([
            String::new(),
            "Your owner's instructions:".to_string(),
            instructions.to_string(),
        ]);
    }
    lines.join("\n")
}

/// The prompt for a bot chat whose bot was deleted: it can see nothing.
pub(super) fn orphan_prompt() -> String {
    "You are an agent bot in Cypher whose bot has been deleted. You can see no projects or \
     sessions; say so if asked."
        .to_string()
}

/// A space's display name: its rename, else its folder's name.
pub(super) fn space_name(space: &Space) -> String {
    space.name.clone().unwrap_or_else(|| {
        std::path::Path::new(&space.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| space.path.clone())
    })
}
