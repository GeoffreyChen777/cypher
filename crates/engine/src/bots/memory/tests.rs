//! The routine tools' permissions by origin, the prompt section and the
//! schedule listing. The scheduler runs against a real engine in
//! `tests/bot_schedule.rs`.

use chrono::{DateTime, Local, TimeZone, Utc};

use cypher_proto::{
    Bot, BotCapabilities, BotChat, BotOrigin, BotRecords, BotRoutine, BotScope, BotThread,
    ProjectGrant, RoutineTarget, RunOutcome, RunRecord, Schedule, Space,
};

use super::{Access, access, memory_prompt, memory_tools, render_routines};

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
fn owner_and_routine_threads_list_routines_and_peers_get_nothing() {
    let main = access(&BotChat::owner("b1"));
    assert!(main.list_routines && main.save_routine);

    let work = BotChat::thread(
        "b1",
        BotOrigin::Owner,
        BotThread::Work {
            work_id: "w1".into(),
        },
    );
    let orchestration = access(&work);
    assert!(orchestration.list_routines);
    assert!(!orchestration.save_routine, "the workspace chat only");

    let routine = access(&routine_thread());
    assert!(routine.list_routines && !routine.save_routine);

    // Peers get nothing, whatever a row claims.
    assert_eq!(access(&peer_thread()), Access::default());
    assert!(memory_tools(&peer_thread(), Some(&bot(true))).is_empty());
    let odd = BotChat::thread("b1", BotOrigin::Peer, BotThread::Main);
    assert_eq!(access(&odd), Access::default());
    let unknown = BotChat::thread("b1", BotOrigin::Owner, BotThread::Unknown);
    assert_eq!(access(&unknown), Access::default());
}

#[test]
fn allowlists_follow_the_access() {
    assert_eq!(
        memory_tools(&BotChat::owner("b1"), Some(&bot(true))),
        cypher_proto::BOT_SCHEDULE_TOOLS
    );
    assert_eq!(
        memory_tools(&routine_thread(), Some(&bot(true))),
        ["list_routines"]
    );
    assert!(memory_tools(&BotChat::owner("b1"), None).is_empty());
}

#[test]
fn only_the_workspace_chat_is_told_how_routines_work() {
    let section = memory_prompt(&BotChat::owner("b1")).unwrap();
    assert!(section.contains("save_routine") && section.contains("list_routines"));
    assert!(memory_prompt(&routine_thread()).is_none());
}

fn local(ms: i64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms).unwrap()
}

#[test]
fn the_schedule_lists_routines_and_where_they_run() {
    let now = Local.with_ymd_and_hms(2026, 10, 19, 8, 0, 0).unwrap();
    let nine = Local.with_ymd_and_hms(2026, 10, 19, 9, 0, 0).unwrap();
    let mut records = BotRecords::default();
    records.routines.push(BotRoutine {
        id: "r1".into(),
        bot_id: "b1".into(),
        name: "Morning brief".into(),
        prompt: "What's stuck?".into(),
        target: RoutineTarget::Workspace,
        model: None,
        schedule: Schedule::Daily {
            time: "09:00".into(),
            days: vec![1, 2, 3, 4, 5],
        },
        enabled: true,
        write_notes: true,
        work_directly: false,
        created_at: 0,
        updated_at: 0,
        run_requested_at: None,
        thread_chat_id: None,
        manual_chat_id: None,
        worktree_path: None,
        last_run: Some(RunRecord {
            at: nine.timestamp_millis() - 3 * 86_400_000,
            ended_at: None,
            outcome: RunOutcome::Failed,
            summary: Some("This run is unattended, so it\ncouldn't ask.".into()),
            chat_id: None,
            manual: false,
        }),
        next_run_at: Some(nine.timestamp_millis()),
    });
    let mut nightly = records.routines[0].clone();
    nightly.id = "r2".into();
    nightly.name = "Nightly tests".into();
    nightly.schedule = Schedule::Manual;
    nightly.write_notes = false;
    nightly.last_run = None;
    nightly.next_run_at = None;
    nightly.target = RoutineTarget::Project {
        space_id: "sp-web".into(),
        worktree: true,
        model: None,
    };
    records.routines.push(nightly);
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
    let text = render_routines(&bot(true), &records, &spaces, |_| "Studio".into(), now);
    assert!(text.starts_with("Times are Studio's local time"), "{text}");
    assert!(
        text.contains(
            "- \"Morning brief\" (id r1) · Weekdays 09:00 · next Mon Oct 19 09:00 · last run \
             Fri Oct 16 09:00 failed: This run is unattended, so it couldn't ask. · runs as you · \
             writes memory"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "- \"Nightly tests\" (id r2) · Manual · runs by hand · never run · a session in web \
             on Studio, in its own worktree"
        ),
        "{text}"
    );

    let mut paused = bot(false);
    paused.paused = true;
    let empty = render_routines(
        &paused,
        &BotRecords::default(),
        &[],
        |_| "Studio".into(),
        local(0),
    );
    assert!(empty.contains("This workspace is paused"), "{empty}");
    assert!(empty.contains("No routines yet."), "{empty}");
}
