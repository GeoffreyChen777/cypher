//! The scheduler's rules as pure functions (docs/design/agent-bots.md §8):
//! when a routine or task is due, what its `nextRunAt` should be, the daily
//! budget, and how far a started run has got. The host loop reads the rows,
//! asks these, and writes back what they say.

use chrono::{DateTime, TimeZone};

use cypher_doc::SessionCommandStatus;
use cypher_proto::{RunOutcome, RunRecord, Schedule, SessionStatus};

/// How long a recorded run may go without its Run command before it counts
/// as never started (the engine stopped between the two writes).
pub(super) const START_GRACE_MS: i64 = 60_000;

/// The `nextRunAt` the host should hold for a job, given the one it holds.
///
/// Manual and disabled jobs have none. A held time stays while it is still
/// one of the schedule's slots and no slot comes before it (the owner did
/// not move the schedule earlier); a slot already past stays too, so a run a
/// sleeping device missed still happens once on wake. Anything else is
/// replaced by the first slot after `now`.
pub(super) fn planned_next<Tz: TimeZone>(
    schedule: &Schedule,
    enabled: bool,
    held: Option<i64>,
    now: &DateTime<Tz>,
) -> Option<i64> {
    if !enabled {
        return None;
    }
    let next = schedule.next_after(now)?.timestamp_millis();
    let Some(held) = held else {
        return Some(next);
    };
    let is_slot = match schedule {
        Schedule::Manual => false,
        Schedule::Interval { .. } => true,
        Schedule::Daily { .. } => now
            .timezone()
            .timestamp_millis_opt(held - 1)
            .single()
            .and_then(|before| schedule.next_after(&before))
            .is_some_and(|slot| slot.timestamp_millis() == held),
    };
    if is_slot && held <= next {
        Some(held)
    } else {
        Some(next)
    }
}

/// Why a job should run now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Due {
    /// The owner asked for a run that hasn't happened yet ("Run now").
    Requested,
    /// Its scheduled time has come (or passed while the device slept).
    Scheduled,
}

/// Whether a job is due at `now_ms`. A request counts until a run starts
/// after it; a schedule counts once `next_run_at` has passed.
pub(super) fn due(
    next_run_at: Option<i64>,
    run_requested_at: Option<i64>,
    last_run: Option<&RunRecord>,
    now_ms: i64,
) -> Option<Due> {
    let requested = run_requested_at
        .is_some_and(|asked| last_run.is_none_or(|run| run.at < asked) && asked <= now_ms);
    if requested {
        Some(Due::Requested)
    } else if next_run_at.is_some_and(|at| at <= now_ms) {
        Some(Due::Scheduled)
    } else {
        None
    }
}

/// Whether a job's last run is still going.
pub(super) fn is_running(last_run: Option<&RunRecord>) -> bool {
    last_run.is_some_and(|run| run.outcome == RunOutcome::Running)
}

/// Runs already made on `today` (the host's local date), from the routine's
/// counter for `runs_day`.
pub(super) fn runs_on(today: &str, runs_today: u32, runs_day: Option<&str>) -> u32 {
    if runs_day == Some(today) {
        runs_today
    } else {
        0
    }
}

/// Why a due routine is skipped instead of run, if it is.
pub(super) fn skip_reason(paused: bool, runs_today: u32, max_runs_per_day: u32) -> Option<String> {
    if paused {
        Some("Skipped: the workspace is paused.".into())
    } else if runs_today >= max_runs_per_day {
        Some(format!(
            "Skipped: today's budget of {max_runs_per_day} run{} is spent.",
            if max_runs_per_day == 1 { "" } else { "s" }
        ))
    } else {
        None
    }
}

/// Why a due project routine is skipped: its bot lost *Coordinate*, or the
/// project left its scope.
pub(super) fn project_skip(coordinate: bool, sees_project: bool) -> Option<String> {
    if !coordinate {
        Some("Skipped: running in a project needs the workspace's Coordinate permission.".into())
    } else if !sees_project {
        Some("Skipped: the workspace no longer has access to this project.".into())
    } else {
        None
    }
}

/// How far a started run has got, from its Run command and the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Progress {
    Waiting,
    /// The run is over. `failed` with the reason when one is known.
    Ended {
        failed: bool,
        reason: Option<String>,
    },
}

/// `command` is the latest Run command queued at or after the run started
/// (its status and resolution); `status` is the session's live status here.
pub(super) fn progress(
    run_at: i64,
    now_ms: i64,
    command: Option<(SessionCommandStatus, Option<&str>)>,
    status: Option<SessionStatus>,
) -> Progress {
    match command {
        None if now_ms - run_at < START_GRACE_MS => Progress::Waiting,
        None => Progress::Ended {
            failed: true,
            reason: Some("The run never started.".into()),
        },
        Some((SessionCommandStatus::Pending, _)) => Progress::Waiting,
        Some((SessionCommandStatus::Applied, _)) => match status {
            Some(SessionStatus::Working | SessionStatus::AwaitingInput) => Progress::Waiting,
            Some(SessionStatus::Errored) => Progress::Ended {
                failed: true,
                reason: None,
            },
            Some(SessionStatus::Idle) | None => Progress::Ended {
                failed: false,
                reason: None,
            },
        },
        Some((_, resolution)) => Progress::Ended {
            failed: true,
            reason: Some(match resolution.filter(|r| !r.trim().is_empty()) {
                Some(why) => format!("The run couldn't start: {why}."),
                None => "The run couldn't start.".into(),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn ms(s: &str) -> i64 {
        at(s).timestamp_millis()
    }

    fn daily(time: &str) -> Schedule {
        Schedule::Daily {
            time: time.into(),
            days: vec![],
        }
    }

    fn run(at: i64, outcome: RunOutcome) -> RunRecord {
        RunRecord {
            at,
            ended_at: None,
            outcome,
            summary: None,
            chat_id: None,
            manual: false,
        }
    }

    #[test]
    fn next_run_is_written_when_missing_and_kept_when_valid() {
        let now = at("2026-10-19T08:00:00+01:00");
        let nine = ms("2026-10-19T09:00:00+01:00");
        assert_eq!(planned_next(&daily("09:00"), true, None, &now), Some(nine));
        assert_eq!(
            planned_next(&daily("09:00"), true, Some(nine), &now),
            Some(nine)
        );
        assert_eq!(planned_next(&daily("09:00"), false, Some(nine), &now), None);
        assert_eq!(
            planned_next(&Schedule::Manual, true, Some(nine), &now),
            None
        );
    }

    #[test]
    fn a_moved_schedule_replaces_a_stale_next_run() {
        let now = at("2026-10-19T08:00:00+01:00");
        let nine = ms("2026-10-19T09:00:00+01:00");
        // 09:00 → 10:00: the held 09:00 is no longer a slot.
        assert_eq!(
            planned_next(&daily("10:00"), true, Some(nine), &now),
            Some(ms("2026-10-19T10:00:00+01:00"))
        );
        // Every 4 h → every hour: an earlier slot comes first.
        let later = ms("2026-10-19T12:00:00+01:00");
        assert_eq!(
            planned_next(&Schedule::Interval { minutes: 60 }, true, Some(later), &now),
            Some(ms("2026-10-19T09:00:00+01:00"))
        );
    }

    #[test]
    fn a_missed_slot_stays_due_once() {
        // Asleep through Saturday's and Sunday's runs; awake Monday 08:00.
        let now = at("2026-10-19T08:00:00+01:00");
        let saturday = ms("2026-10-17T09:00:00+01:00");
        let held = planned_next(&daily("09:00"), true, Some(saturday), &now);
        assert_eq!(held, Some(saturday), "the missed slot is kept");
        assert_eq!(
            due(held, None, None, now.timestamp_millis()),
            Some(Due::Scheduled)
        );
        // After the run, the next one is planned from now: the rest are skipped.
        assert_eq!(
            planned_next(&daily("09:00"), true, None, &now),
            Some(ms("2026-10-19T09:00:00+01:00"))
        );
    }

    #[test]
    fn requests_count_until_a_run_starts_after_them() {
        let now = 10_000;
        assert_eq!(due(None, Some(5_000), None, now), Some(Due::Requested));
        let earlier = run(4_000, RunOutcome::Done);
        assert_eq!(
            due(None, Some(5_000), Some(&earlier), now),
            Some(Due::Requested)
        );
        let after = run(6_000, RunOutcome::Done);
        assert_eq!(due(None, Some(5_000), Some(&after), now), None);
        assert_eq!(due(Some(20_000), None, None, now), None);
        assert_eq!(due(Some(9_000), None, None, now), Some(Due::Scheduled));
    }

    #[test]
    fn project_routines_need_coordinate_and_their_project() {
        assert_eq!(project_skip(true, true), None);
        assert!(project_skip(false, true).unwrap().contains("Coordinate"));
        assert!(
            project_skip(true, false)
                .unwrap()
                .contains("no longer has access")
        );
    }

    #[test]
    fn budgets_and_pauses_skip_runs() {
        assert_eq!(runs_on("2026-10-19", 3, Some("2026-10-19")), 3);
        assert_eq!(runs_on("2026-10-19", 3, Some("2026-10-18")), 0, "a new day");
        assert_eq!(runs_on("2026-10-19", 3, None), 0);
        assert_eq!(skip_reason(false, 2, 3), None);
        assert_eq!(
            skip_reason(false, 3, 3).as_deref(),
            Some("Skipped: today's budget of 3 runs is spent.")
        );
        assert_eq!(
            skip_reason(true, 0, 3).as_deref(),
            Some("Skipped: the workspace is paused.")
        );
        assert!(is_running(Some(&run(1, RunOutcome::Running))));
        assert!(!is_running(Some(&run(1, RunOutcome::Failed))));
    }

    #[test]
    fn progress_follows_the_command_then_the_session() {
        use SessionCommandStatus as C;
        assert_eq!(progress(0, 1_000, None, None), Progress::Waiting);
        assert!(matches!(
            progress(0, START_GRACE_MS, None, None),
            Progress::Ended { failed: true, .. }
        ));
        assert_eq!(
            progress(0, 1_000, Some((C::Pending, None)), None),
            Progress::Waiting
        );
        assert_eq!(
            progress(
                0,
                1_000,
                Some((C::Applied, None)),
                Some(SessionStatus::Working)
            ),
            Progress::Waiting
        );
        assert_eq!(
            progress(
                0,
                1_000,
                Some((C::Applied, None)),
                Some(SessionStatus::Idle)
            ),
            Progress::Ended {
                failed: false,
                reason: None
            }
        );
        assert_eq!(
            progress(
                0,
                1_000,
                Some((C::Applied, None)),
                Some(SessionStatus::Errored)
            ),
            Progress::Ended {
                failed: true,
                reason: None
            }
        );
        assert_eq!(
            progress(
                0,
                1_000,
                Some((C::Rejected, Some("worktree create failed"))),
                None
            ),
            Progress::Ended {
                failed: true,
                reason: Some("The run couldn't start: worktree create failed.".into())
            }
        );
    }
}
