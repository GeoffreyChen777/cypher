//! The notebook's permissions by origin, the prompt section and the
//! schedule listing. The scheduler runs against a real engine in
//! `tests/bot_schedule.rs`.

use chrono::{DateTime, Local, TimeZone, Utc};

use cypher_proto::{
    Bot, BotCapabilities, BotChat, BotNote, BotOrigin, BotRecords, BotRoutine, BotScope, BotThread,
    NoteSource, ProjectGrant, ProjectTask, RunOutcome, RunRecord, Schedule, Space,
};

use super::{Access, access, memory_tools, notebook_section, render_schedule};

fn bot(coordinate: bool) -> Bot {
    Bot {
        id: "b1".into(),
        name: "Bob".into(),
        icon: None,
        color: None,
        host_device_id: "dev".into(),
        chat_id: "bot-chat".into(),
        instructions: None,
        scope: BotScope {
            projects: ProjectGrant::Selected {
                space_ids: vec!["sp-web".into()],
            },
            ..Default::default()
        },
        capabilities: BotCapabilities {
            coordinate,
            ..Default::default()
        },
        paused: false,
        created_at: Utc::now(),
    }
}

fn routine_thread() -> BotChat {
    BotChat::thread(
        "b1",
        BotOrigin::Routine,
        BotThread::Routine {
            routine_id: "r1".into(),
        },
    )
}

fn peer_thread() -> BotChat {
    BotChat::thread(
        "b1",
        BotOrigin::Peer,
        BotThread::Peer {
            connection_id: "c1".into(),
        },
    )
}

#[test]
fn owner_threads_keep_the_notebook_and_peers_never_see_it() {
    let main = access(&BotChat::owner("b1"), &bot(false), false);
    assert!(main.read_notes && main.write_notes && main.list_schedule && main.save_routine);
    assert!(!main.save_task, "tasks start sessions: Coordinate only");
    assert!(access(&BotChat::owner("b1"), &bot(true), false).save_task);

    let work = BotChat::thread(
        "b1",
        BotOrigin::Owner,
        BotThread::Work {
            work_id: "w1".into(),
        },
    );
    let orchestration = access(&work, &bot(true), false);
    assert!(orchestration.read_notes && orchestration.write_notes);
    assert!(
        !orchestration.save_routine && !orchestration.save_task,
        "the workspace chat only"
    );

    let routine = access(&routine_thread(), &bot(true), false);
    assert!(routine.read_notes && routine.list_schedule);
    assert!(!routine.write_notes, "only when the routine says so");
    assert!(access(&routine_thread(), &bot(true), true).write_notes);
    assert!(!routine.save_routine && !routine.save_task);

    // Peers get nothing, whatever a row claims.
    assert_eq!(access(&peer_thread(), &bot(true), true), Access::default());
    assert!(memory_tools(&peer_thread(), Some(&bot(true))).is_empty());
    let odd = BotChat::thread("b1", BotOrigin::Peer, BotThread::Main);
    assert_eq!(access(&odd, &bot(true), true), Access::default());
    let unknown = BotChat::thread("b1", BotOrigin::Owner, BotThread::Unknown);
    assert_eq!(access(&unknown, &bot(true), true), Access::default());
}

#[test]
fn allowlists_follow_the_access() {
    assert_eq!(
        memory_tools(&BotChat::owner("b1"), Some(&bot(true))),
        cypher_proto::BOT_MEMORY_TOOLS
    );
    assert_eq!(
        memory_tools(&BotChat::owner("b1"), Some(&bot(false))),
        ["read_notes", "write_note", "list_schedule", "save_routine"]
    );
    // The call checks the routine's write_notes; the allowlist can't.
    assert_eq!(
        memory_tools(&routine_thread(), Some(&bot(true))),
        ["read_notes", "write_note", "list_schedule"]
    );
    assert!(memory_tools(&BotChat::owner("b1"), None).is_empty());
}

fn note(id: &str, text: &str, at: i64, source: NoteSource) -> BotNote {
    BotNote {
        id: id.into(),
        bot_id: "b1".into(),
        text: text.into(),
        source,
        created_at: at,
        updated_at: at,
    }
}

#[test]
fn the_prompt_carries_the_newest_notes() {
    let owner = note("n1", "Alice owns auth", 2_000, NoteSource::Owner);
    let bot_note = note(
        "n2",
        "The release is\nblocked on the migration",
        1_000,
        NoteSource::Bot {
            chat_id: Some("bot-chat".into()),
            title: Some("Bob".into()),
        },
    );
    let main = access(&BotChat::owner("b1"), &bot(false), false);
    let section = notebook_section(&[&owner, &bot_note], main, true);
    assert!(section.starts_with("Your notebook:"), "{section}");
    assert!(section.contains("write_note"), "{section}");
    assert!(section.contains("save_routine"), "{section}");
    let alice = section
        .find("- Alice owns auth (from your owner, ")
        .unwrap();
    let release = section
        .find("- The release is blocked on the migration (you, in \"Bob\", ")
        .unwrap();
    assert!(alice < release, "newest first: {section}");

    let reader = access(&routine_thread(), &bot(false), false);
    let section = notebook_section(&[], reader, false);
    assert!(section.contains("may read them but not write"), "{section}");
    assert!(section.contains("(Your notebook is empty.)"), "{section}");
    assert!(
        !section.contains("save_routine"),
        "only the workspace chat: {section}"
    );

    let many: Vec<BotNote> = (0..cypher_proto::NOTES_IN_PROMPT + 5)
        .map(|i| {
            note(
                &format!("n{i}"),
                &format!("fact {i}"),
                i as i64,
                NoteSource::Owner,
            )
        })
        .collect();
    let refs: Vec<&BotNote> = many.iter().collect();
    let section = notebook_section(&refs, main, true);
    assert!(section.contains(&format!(
        "The newest {} of {} notes are below; read_notes finds the rest.",
        cypher_proto::NOTES_IN_PROMPT,
        many.len()
    )));
    assert_eq!(
        section.lines().filter(|l| l.starts_with("- fact")).count(),
        cypher_proto::NOTES_IN_PROMPT
    );
}

fn local(ms: i64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms).unwrap()
}

#[test]
fn the_schedule_lists_routines_and_the_tasks_of_granted_projects() {
    let now = Local.with_ymd_and_hms(2026, 10, 19, 8, 0, 0).unwrap();
    let nine = Local.with_ymd_and_hms(2026, 10, 19, 9, 0, 0).unwrap();
    let mut records = BotRecords::default();
    records.routines.push(BotRoutine {
        id: "r1".into(),
        bot_id: "b1".into(),
        name: "Morning brief".into(),
        prompt: "What's stuck?".into(),
        schedule: Schedule::Daily {
            time: "09:00".into(),
            days: vec![1, 2, 3, 4, 5],
        },
        enabled: true,
        write_notes: true,
        work_directly: false,
        max_runs_per_day: 24,
        created_at: 0,
        updated_at: 0,
        run_requested_at: None,
        thread_chat_id: None,
        last_run: Some(RunRecord {
            at: nine.timestamp_millis() - 3 * 86_400_000,
            ended_at: None,
            outcome: RunOutcome::Failed,
            summary: Some("This run is unattended, so it\ncouldn't ask.".into()),
            chat_id: None,
        }),
        next_run_at: Some(nine.timestamp_millis()),
        runs_today: 0,
        runs_day: None,
    });
    let task = |id: &str, space: &str| ProjectTask {
        id: id.into(),
        space_id: space.into(),
        name: format!("{id} task"),
        prompt: "Run the tests".into(),
        schedule: Schedule::Manual,
        enabled: true,
        worktree: true,
        model: None,
        created_by: None,
        created_at: 0,
        updated_at: 0,
        run_requested_at: None,
        last_run: None,
        next_run_at: None,
    };
    records.tasks = vec![task("nightly", "sp-web"), task("hidden", "sp-secret")];
    let space = |id: &str, path: &str| Space {
        id: id.into(),
        device_id: "dev".into(),
        path: path.into(),
        name: None,
        git_detected: true,
        git_checked_at: None,
        checkout_id: None,
        created_at: Utc::now(),
        pinned: false,
        icon: None,
        color: None,
    };
    let spaces = [
        space("sp-web", "/code/web"),
        space("sp-secret", "/code/secret"),
    ];
    let text = render_schedule(&bot(true), &records, &spaces, |_| "Studio".into(), now);
    assert!(text.starts_with("Times are Studio's local time"), "{text}");
    assert!(
        text.contains(
            "- \"Morning brief\" (id r1) · Weekdays 09:00 · next Mon Oct 19 09:00 · last run \
             Fri Oct 16 09:00 failed: This run is unattended, so it couldn't ask. · writes notes"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "- \"nightly task\" in web on Studio (id nightly) · Manual · runs by hand · never \
             run · new worktree each run"
        ),
        "{text}"
    );
    assert!(!text.contains("hidden"), "out of scope: {text}");

    let mut paused = bot(false);
    paused.paused = true;
    let empty = render_schedule(
        &paused,
        &BotRecords::default(),
        &[],
        |_| "Studio".into(),
        local(0),
    );
    assert!(empty.contains("This workspace is paused"), "{empty}");
    assert!(empty.contains("No routines yet."), "{empty}");
    assert!(
        empty.contains("No recurring tasks in your projects."),
        "{empty}"
    );
}
