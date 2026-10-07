use super::*;
use cypher_proto::ToolCall;

fn ms(epoch_secs: i64) -> i64 {
    epoch_secs * 1000
}

#[test]
fn running_spinner_advances_and_wraps() {
    assert_eq!(running_spinner_glyph(0.0), "⠋");
    assert_eq!(running_spinner_glyph(0.15), "⠙");
    assert_eq!(running_spinner_glyph(0.95), "⠏");
    assert_eq!(running_spinner_glyph(1.0), "⠋");
}

fn subagent_part(id: &str, is_async: bool, resolved: bool, is_error: bool) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call: ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({
                "agent": if id.starts_with("planner") { "planner" } else { "actor" },
                "task": "Plan the panel",
                "async": is_async,
            })),
        },
        is_error,
        resolved,
        output: None,
        progress: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
    }
}

fn entry(parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "m1".into(),
        role: cypher_doc::MessageRole::Assistant,
        parts,
        created_at: ms(1000),
        device_id: "d".into(),
        ..crate::test_fixtures::entry()
    }
}

/// A settled entry (`status: None`) — the common case for a finished
/// assistant message. An entry mid-stream is marked Streaming.
fn streaming_entry(parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        status: Some(MessageStatus::Streaming),
        ..entry(parts)
    }
}

fn run(
    run_id: &str,
    tool_call_id: Option<&str>,
    mode: SubagentRunMode,
    status: SubagentRunStatus,
    updated_at: i64,
) -> SubagentRun {
    SubagentRun {
        run_id: run_id.into(),
        tool_call_id: tool_call_id.map(str::to_owned),
        agent: "planner".into(),
        model: Some("anthropic/claude-sonnet-4".into()),
        task: "Plan the panel".into(),
        mode,
        status,
        progress: Some("live tail".into()),
        started_at: ms(1000),
        updated_at,
        ended_at: if status == SubagentRunStatus::Running {
            None
        } else {
            Some(updated_at)
        },
        child_chat_id: None,
    }
}

/// Cross-turn aggregation: subagent tool parts from EVERY transcript
/// entry merge into one panel (not just the last turn).
#[test]
fn aggregates_across_turns() {
    // Turn 1: an unresolved sync part on a STILL-STREAMING entry → Starting.
    // Turn 2: a settled resolved sync part → Done.
    let entries = vec![
        streaming_entry(vec![subagent_part("t1", false, false, false)]),
        entry(vec![subagent_part("t2", false, true, false)]),
    ];
    let now = ms(2000);
    let out = aggregate_subagents(&entries, &[], now);
    assert_eq!(out.len(), 2);
    let t1 = out
        .iter()
        .find(|e| e.tool_call_id.as_deref() == Some("t1"))
        .unwrap();
    assert_eq!(t1.status, PanelStatus::Starting);
    let t2 = out
        .iter()
        .find(|e| e.tool_call_id.as_deref() == Some("t2"))
        .unwrap();
    assert_eq!(t2.status, PanelStatus::Done);
}

/// Sync runs: the doc ToolResult terminal state wins even when the
/// snapshot still says running; the snapshot supplements model/progress.
#[test]
fn sync_doc_terminal_wins_snapshot_supplements() {
    let entries = vec![entry(vec![subagent_part("t1", false, true, false)])];
    let now = ms(2000);
    let snapshot = vec![run(
        "r1",
        Some("t1"),
        SubagentRunMode::Sync,
        SubagentRunStatus::Running,
        now - 1000,
    )];
    let out = aggregate_subagents(&entries, &snapshot, ms(2000));
    let t1 = &out[0];
    // Doc says resolved+ok → Done, despite the snapshot still Running.
    assert_eq!(t1.status, PanelStatus::Done);
    assert_eq!(t1.model.as_deref(), Some("anthropic/claude-sonnet-4"));
    assert_eq!(t1.progress.as_deref(), Some("live tail"));
}

/// Async runs: the snapshot is the authority. A resolved async launch ack
/// is IGNORED on its own; only the snapshot's Running/Done/Error reads.
#[test]
fn async_snapshot_is_authoritative_never_doc_ack() {
    let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
    let now = ms(2000);
    // Snapshot says running → Running (the doc ToolResult was only the ack).
    let snapshot = vec![run(
        "r1",
        Some("t1"),
        SubagentRunMode::Async,
        SubagentRunStatus::Running,
        now - 1000,
    )];
    let out = aggregate_subagents(&entries, &snapshot, now);
    assert_eq!(out[0].status, PanelStatus::Running);
    // Snapshot says done → Done.
    let snapshot = vec![run(
        "r1",
        Some("t1"),
        SubagentRunMode::Async,
        SubagentRunStatus::Done,
        now - 1000,
    )];
    let out = aggregate_subagents(&entries, &snapshot, now);
    assert_eq!(out[0].status, PanelStatus::Done);
    // Snapshot says error → Error.
    let snapshot = vec![run(
        "r1",
        Some("t1"),
        SubagentRunMode::Async,
        SubagentRunStatus::Error,
        now - 1000,
    )];
    let out = aggregate_subagents(&entries, &snapshot, now);
    assert_eq!(out[0].status, PanelStatus::Error);
}

/// A snapshot's Running whose freshness went quiet reads stale — the 45s
/// window is a visual heartbeat guard, never a terminal judgment.
#[test]
fn stale_running_reads_stale() {
    let now = ms(2000);
    let entries = vec![streaming_entry(vec![subagent_part(
        "t1", true, false, false,
    )])];
    let snapshot = vec![run(
        "r1",
        Some("t1"),
        SubagentRunMode::Async,
        SubagentRunStatus::Running,
        ms(1000) - SUBAGENT_STALE_MS - 1000,
    )];
    let out = aggregate_subagents(&entries, &snapshot, now);
    assert_eq!(out[0].status, PanelStatus::Stale);
    // A fresh snapshot stays Running.
    let snapshot = vec![run(
        "r1",
        Some("t1"),
        SubagentRunMode::Async,
        SubagentRunStatus::Running,
        now,
    )];
    let out = aggregate_subagents(&entries, &snapshot, now);
    assert_eq!(out[0].status, PanelStatus::Running);
}

/// Snapshot merge by tool call id; message-mode runs (no tool call id)
/// insert by run id alongside.
#[test]
fn snapshot_merges_by_tool_call_and_inserts_message_runs() {
    let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
    let now = ms(2000);
    let snapshot = vec![
        run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        ),
        run(
            "msg-1",
            None,
            SubagentRunMode::Message,
            SubagentRunStatus::Running,
            now,
        ),
        run(
            "ghost",
            Some("t-gone"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        ),
    ];
    let out = aggregate_subagents(&entries, &snapshot, now);
    assert_eq!(out.len(), 3, "merged + message + orphan snapshot runs");
    assert!(
        out.iter()
            .any(|e| e.run_id == "msg-1" && e.mode == SubagentRunMode::Message)
    );
    assert!(out.iter().any(|e| e.run_id == "ghost"));
    assert_eq!(
        out.iter().find(|e| e.run_id == "r1").unwrap().status,
        PanelStatus::Running
    );
}

/// Missing task/agent degrade: a subagent part without agent yields no
/// entry; a blank task still renders.
#[test]
fn missing_fields_do_not_panic() {
    let entries = vec![streaming_entry(vec![MessagePart::Tool {
        id: "t1".into(),
        call: ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({ "agent": "planner" })),
        },
        is_error: false,
        resolved: false,
        output: None,
        progress: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
    }])];
    let out = aggregate_subagents(&entries, &[], ms(2000));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].task, "");
    assert_eq!(out[0].status, PanelStatus::Starting);
    // Non-subagent tool parts never produce rows.
    let entries = vec![entry(vec![MessagePart::Tool {
        id: "t2".into(),
        call: ToolCall::Exec {
            command: "ls".into(),
        },
        is_error: false,
        resolved: true,
        output: None,
        progress: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
    }])];
    assert!(aggregate_subagents(&entries, &[], ms(2000)).is_empty());
}

/// Ordering: in-flight rows first (by started_at), then settled by ended_at
/// desc. The inspector SCROLLS — nothing is truncated away.
#[test]
fn ordering_puts_running_first_and_keeps_everything() {
    let entries = vec![
        entry(vec![subagent_part("a", false, true, false)]),
        entry(vec![subagent_part("b", false, true, false)]),
        entry(vec![subagent_part("c", true, true, false)]),
    ];
    // c is running (async + snapshot running); a and b are settled sync.
    let snapshot = vec![
        run(
            "r-c",
            Some("c"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            ms(2000) - 1000,
        ),
        run(
            "r-a",
            Some("a"),
            SubagentRunMode::Sync,
            SubagentRunStatus::Done,
            ms(2000) - 2000,
        ),
        run(
            "r-b",
            Some("b"),
            SubagentRunMode::Sync,
            SubagentRunStatus::Done,
            ms(2000) - 1000,
        ),
    ];
    let out = aggregate_subagents(&entries, &snapshot, ms(2000));
    let ordered = order_panel(out.clone());
    assert_eq!(ordered.len(), 3, "no truncation");
    assert_eq!(ordered[0].run_id, "r-c", "running first");
    // Settled by ended_at desc.
    assert_eq!(ordered[1].run_id, "r-b");
    assert_eq!(ordered[2].run_id, "r-a");

    // Many more settled rows: ALL survive, ordered, running still first.
    let mut more = out.clone();
    for i in 0..4 {
        more.push(SubagentPanelEntry {
            run_id: format!("extra-{i}"),
            tool_call_id: Some(format!("e{i}")),
            agent: "actor".into(),
            task: "T".into(),
            model: None,
            mode: SubagentRunMode::Sync,
            status: PanelStatus::Done,
            progress: None,
            started_at: ms(1000),
            updated_at: ms(1000),
            ended_at: Some(ms(2000) - 3000 + 200 * i as i64),
            child_chat_id: None,
        });
    }
    let ordered = order_panel(more);
    assert_eq!(ordered.len(), 7, "all rows kept for the scroller");
    assert!(SubagentPanelEntry::in_flight(ordered[0].status));
    // Settled block still ends at the OLDEST settled row.
    assert_eq!(ordered.last().unwrap().run_id, "extra-0");
}

#[test]
fn counts_aggregate_running_done_failed() {
    let entries = vec![
        entry(vec![subagent_part("a", false, true, true)]),
        entry(vec![subagent_part("b", false, true, false)]),
        entry(vec![subagent_part("c", true, true, false)]),
    ];
    let snapshot = vec![run(
        "r-c",
        Some("c"),
        SubagentRunMode::Async,
        SubagentRunStatus::Running,
        ms(2000) - 1000,
    )];
    let out = aggregate_subagents(&entries, &snapshot, ms(2000));
    let counts = panel_counts(&out);
    assert_eq!(counts.running, 1);
    assert_eq!(counts.done, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.stale, 0);
    assert_eq!(counts.starting, 0);
}

/// The split counts distinguish stale runners, starting runs and settled
/// work — the trigger/header copy relies on that distinction.
#[test]
fn counts_split_stale_starting_and_done() {
    let entries = vec![
        entry(vec![subagent_part("a", true, true, false)]),
        entry(vec![subagent_part("b", true, true, false)]),
        streaming_entry(vec![subagent_part("c", true, false, false)]),
        entry(vec![subagent_part("d", false, true, false)]),
        entry(vec![subagent_part("e", false, true, true)]),
    ];
    let now = ms(2000);
    let snapshot = vec![
        // a: fresh running.
        run(
            "r-a",
            Some("a"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        ),
        // b: quiet → stale.
        run(
            "r-b",
            Some("b"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now - SUBAGENT_STALE_MS - 1000,
        ),
    ];
    let out = aggregate_subagents(&entries, &snapshot, now);
    let counts = panel_counts(&out);
    assert_eq!(counts.running, 1);
    assert_eq!(counts.stale, 1);
    // c is unresolved + streaming with no snapshot — Starting, never Done.
    assert_eq!(counts.starting, 1);
    assert_eq!(counts.done, 1);
    assert_eq!(counts.failed, 1);
}

/// Trigger copy priority: real running leads (starting/stale/failed are
/// appended, never folded into the running number), then only-starting,
/// then only-stale, then failed-only, then all-settled.
#[test]
fn trigger_label_priorities() {
    // Live running + starting + stale lead with `●`; failures appended.
    let counts = PanelCounts {
        running: 2,
        stale: 1,
        starting: 1,
        done: 3,
        failed: 1,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "●");
    assert_eq!(label, "2 running · 1 starting · 1 stale · ! 1");

    // No running but starting → faint `◌` (in-flight, never Done).
    let counts = PanelCounts {
        running: 0,
        stale: 1,
        starting: 2,
        done: 1,
        failed: 1,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "◌");
    assert_eq!(label, "2 starting · 1 stale · ! 1");

    // Only stale → warning.
    let counts = PanelCounts {
        running: 0,
        stale: 2,
        starting: 0,
        done: 1,
        failed: 0,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "⚠");
    assert_eq!(label, "2 stale");

    // No in-flight, but failures → danger.
    let counts = PanelCounts {
        running: 0,
        stale: 0,
        starting: 0,
        done: 0,
        failed: 3,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "✕");
    assert_eq!(label, "3 failed");

    // Fully settled → success.
    let counts = PanelCounts {
        running: 0,
        stale: 0,
        starting: 0,
        done: 5,
        failed: 0,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "✓");
    assert_eq!(label, "5 subagents");

    // Mixed: running + failures keeps the running lead with the appended
    // failure count — the running number never absorbs starting/stale.
    let counts = PanelCounts {
        running: 1,
        stale: 0,
        starting: 2,
        done: 2,
        failed: 1,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "●");
    assert_eq!(label, "1 running · 2 starting · ! 1");
}

/// Starting counts as in-flight in the trigger — but with no snapshot it
/// reads as `◌ N starting`, never as running, never as Done.
#[test]
fn starting_is_in_flight_not_running_in_trigger() {
    let counts = PanelCounts {
        running: 0,
        stale: 0,
        starting: 1,
        done: 4,
        failed: 0,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "◌", "no snapshot → starting, not running");
    assert_eq!(label, "1 starting");
}

/// Test 1 — a resolved async launch ACK with NO snapshot aggregates to
/// nothing: the doc-only ack must never ghost a run.
#[test]
fn resolved_async_ack_without_snapshot_is_ignored() {
    let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
    let out = aggregate_subagents(&entries, &[], ms(2000));
    assert!(out.is_empty(), "a launch ack with no snapshot is no run");
}

/// Test 2 — an unresolved part on a STILL-STREAMING entry reads Starting
/// and never contributes to the running count.
#[test]
fn streaming_unresolved_reads_starting_not_running() {
    let entries = vec![streaming_entry(vec![subagent_part(
        "t1", false, false, false,
    )])];
    let out = aggregate_subagents(&entries, &[], ms(2000));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].status, PanelStatus::Starting);
    let counts = panel_counts(&out);
    assert_eq!(counts.running, 0);
    assert_eq!(counts.starting, 1);
}

/// Test 3 — an unresolved part on a SETTLED entry (Complete/Aborted) is
/// ignored: a dead turn must not ghost a runner.
#[test]
fn nonstreaming_unresolved_is_ignored() {
    let entries = vec![entry(vec![subagent_part("t1", false, false, false)])];
    let out = aggregate_subagents(&entries, &[], ms(2000));
    assert!(out.is_empty(), "settled entry's unresolved part is a ghost");
}

/// Test 4 — a resolved async launch that FAILED (is_error) is a durable
/// Error even with no snapshot: the launch itself failed.
#[test]
fn resolved_async_error_is_durable_error_fallback() {
    let entries = vec![entry(vec![subagent_part("t1", true, true, true)])];
    let out = aggregate_subagents(&entries, &[], ms(2000));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].status, PanelStatus::Error);
    let counts = panel_counts(&out);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.running, 0);
}

/// Test 5 — Running/Stale can ONLY come from a structured snapshot: the
/// same doc part is Starting without one and Running with one.
#[test]
fn running_only_comes_from_snapshot() {
    let entries = vec![streaming_entry(vec![subagent_part(
        "t1", true, false, false,
    )])];
    // No snapshot: Starting, never Running.
    let out = aggregate_subagents(&entries, &[], ms(2000));
    assert_eq!(out[0].status, PanelStatus::Starting);
    assert_eq!(panel_counts(&out).running, 0);
    // Fresh snapshot on the same tool call: Running.
    let now = ms(2000);
    let out = aggregate_subagents(
        &entries,
        &[run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        )],
        now,
    );
    assert_eq!(out[0].status, PanelStatus::Running);
    assert_eq!(panel_counts(&out).running, 1);
}

/// Test 6 — async snapshot Running/Done/Error is the authority over the
/// doc ack (covered across three snapshots in
/// [`async_snapshot_is_authoritative_never_doc_ack`]).
#[test]
fn async_snapshot_authority_covers_all_terminal_statuses() {
    let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
    let now = ms(2000);
    for (status, expected) in [
        (SubagentRunStatus::Running, PanelStatus::Running),
        (SubagentRunStatus::Done, PanelStatus::Done),
        (SubagentRunStatus::Error, PanelStatus::Error),
    ] {
        let out = aggregate_subagents(
            &entries,
            &[run("r1", Some("t1"), SubagentRunMode::Async, status, now)],
            now,
        );
        assert_eq!(
            out[0].status, expected,
            "async snapshot {status:?} is authority"
        );
    }
}

/// Test 7 — when a snapshot goes from running to EMPTY, the async ack is
/// NOT resurrected: the aggregate is pure over the current inputs.
#[test]
fn snapshot_disappearing_does_not_resurrect_ack() {
    let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
    let now = ms(2000);
    let with_running = aggregate_subagents(
        &entries,
        &[run(
            "r1",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        )],
        now,
    );
    assert_eq!(with_running[0].status, PanelStatus::Running);
    // The same transcript with the snapshot gone → no run at all.
    let without = aggregate_subagents(&entries, &[], now);
    assert!(without.is_empty(), "an ack must not outlive its snapshot");
}

/// Test 8 — the trigger's running number is exactly the real Running
/// count; starting/stale are appended, never folded in.
#[test]
fn trigger_running_number_excludes_starting_and_stale() {
    let counts = PanelCounts {
        running: 1,
        stale: 3,
        starting: 2,
        done: 1,
        failed: 0,
    };
    let (glyph, label) = trigger_label(&counts);
    assert_eq!(glyph, "●");
    assert_eq!(label, "1 running · 2 starting · 3 stale");
}

/// Chat switch closes the old inspector (the reducer the state
/// observation drives).
#[test]
fn chat_switch_closes_the_old_inspector() {
    assert!(chat_switch_closes(Some("a"), Some("b")));
    assert!(chat_switch_closes(Some("a"), None));
    assert!(!chat_switch_closes(Some("a"), Some("a")));
    assert!(!chat_switch_closes(None, Some("b")));
    assert!(!chat_switch_closes(None, None));
}

/// The inspector's call-field parsing reads agent/task/async off the
/// doc tool part (the privacy-safe reader shared with the transcript).
#[test]
fn subagent_call_info_parses_the_panel_fields() {
    let call = ToolCall::Unknown {
        name: "subagent".into(),
        input: Some(serde_json::json!({ "agent": "planner", "task": "Plan\nmore", "async": true })),
    };
    let info = subagent_call_info(&call).expect("extracts");
    assert_eq!(info.agent, "planner");
    assert_eq!(info.task, "Plan\nmore");
    assert!(info.is_async);
}

/// The Cypher child chat id rides the snapshot run into the panel entry:
/// a run with `childChatId` reads navigable (the row opens the child's
/// live session), and a run without one never does (standalone spawns).
#[test]
fn child_chat_id_propagates_and_marks_navigable() {
    let entries = vec![entry(vec![subagent_part("t1", true, false, false)])];
    let now = ms(2000);
    let mut navigable = run(
        "r1",
        Some("t1"),
        SubagentRunMode::Async,
        SubagentRunStatus::Running,
        now,
    );
    navigable.child_chat_id = Some("child-1".into());
    let out = aggregate_subagents(&entries, &[navigable], now);
    assert_eq!(out.len(), 1);
    assert!(out[0].is_navigable());
    assert_eq!(out[0].child_chat_id.as_deref(), Some("child-1"));

    // A plain run (no child chat) stays non-navigable.
    let out = aggregate_subagents(
        &entries,
        &[run(
            "r2",
            Some("t1"),
            SubagentRunMode::Async,
            SubagentRunStatus::Running,
            now,
        )],
        now,
    );
    assert_eq!(out[0].child_chat_id, None);
    assert!(!out[0].is_navigable());

    // Snapshot-only runs (no transcript part) keep the id too.
    let out = aggregate_subagents(
        &[],
        &[SubagentRun {
            run_id: "r3".into(),
            tool_call_id: None,
            agent: "planner".into(),
            model: None,
            task: "T".into(),
            mode: SubagentRunMode::Message,
            status: SubagentRunStatus::Running,
            progress: None,
            started_at: now,
            updated_at: now,
            ended_at: None,
            child_chat_id: Some("child-3".into()),
        }],
        now,
    );
    assert_eq!(out[0].child_chat_id.as_deref(), Some("child-3"));
    assert!(out[0].is_navigable());
}

// ---- durable child rows (CRITICAL: reopenability/status truth) ----

fn child_row(
    chat_id: &str,
    parent_run_id: &str,
    tool_call_id: Option<&str>,
    created_at: i64,
) -> DurableChildRow {
    DurableChildRow {
        chat_id: chat_id.into(),
        parent_run_id: parent_run_id.into(),
        tool_call_id: tool_call_id.map(str::to_owned),
        agent: "planner".into(),
        task: "Plan the panel".into(),
        mode: SubagentRunMode::Async,
        created_at_ms: created_at,
    }
}

/// A completed child stays navigable after an EMPTY parent snapshot: the
/// durable child row synthesizes the run (child session Idle → Done).
#[test]
fn child_rows_keep_completed_children_navigable_after_empty_snapshot() {
    let now = ms(2000);
    // Async launch ack with no snapshot: the plain aggregate drops it.
    let entries = vec![entry(vec![subagent_part("t1", true, true, false)])];
    let base = aggregate_subagents(&entries, &[], now);
    assert!(base.is_empty(), "an ack must not outlive its snapshot");
    // The durable child row restores the run and keeps it clickable.
    let children = vec![child_row("child-1", "run-1", Some("t1"), ms(1000))];
    let out = merge_child_rows(base, &children, |_| Some(PanelStatus::Done));
    assert_eq!(out.len(), 1);
    assert!(out[0].is_navigable());
    assert_eq!(out[0].child_chat_id.as_deref(), Some("child-1"));
    assert_eq!(out[0].run_id, "run-1");
    assert_eq!(out[0].status, PanelStatus::Done);
    assert_eq!(out[0].tool_call_id.as_deref(), Some("t1"));
}

/// After a parent RESTART the sync doc row (run id = tool call id) links
/// to the durable child by `tool_call_id` — exactly ONE navigable row,
/// never a doc row plus a synthesized twin.
#[test]
fn child_rows_link_doc_rows_to_children_after_restart() {
    let now = ms(2000);
    let entries = vec![entry(vec![subagent_part("t1", false, true, false)])];
    let base = aggregate_subagents(&entries, &[], now);
    assert_eq!(base.len(), 1);
    assert_eq!(base[0].run_id, "t1", "doc-only row keys by tool call id");
    assert!(!base[0].is_navigable());
    let children = vec![child_row("child-1", "run-1", Some("t1"), ms(1000))];
    let out = merge_child_rows(base, &children, |_| Some(PanelStatus::Done));
    assert_eq!(out.len(), 1, "no duplicate row for one child");
    assert_eq!(out[0].run_id, "run-1", "renamed to the durable run id");
    assert!(out[0].is_navigable());
    assert_eq!(out[0].child_chat_id.as_deref(), Some("child-1"));
    assert_eq!(out[0].status, PanelStatus::Done);
}

/// The child's OWN session row is the execution truth for a merged row.
#[test]
fn child_rows_use_child_session_as_execution_truth() {
    let children = vec![child_row("child-1", "run-1", None, ms(1000))];
    assert_eq!(
        merge_child_rows(vec![], &children, |_| Some(PanelStatus::Running))[0].status,
        PanelStatus::Running
    );
    assert_eq!(
        merge_child_rows(vec![], &children, |_| Some(PanelStatus::Error))[0].status,
        PanelStatus::Error
    );
    assert_eq!(
        merge_child_rows(vec![], &children, |_| Some(PanelStatus::Done))[0].status,
        PanelStatus::Done
    );
    // No session row yet → the run is still in-flight (queued), never a
    // false terminal.
    assert_eq!(
        merge_child_rows(vec![], &children, |_| None)[0].status,
        PanelStatus::Starting
    );
}

/// The durable relation wins on a matched row, and the child session
/// overrides a stale parent-side status (e.g. a leftover Running edge).
#[test]
fn child_rows_durable_relation_wins_and_session_overrides_status() {
    let now = ms(2000);
    let entries = vec![entry(vec![subagent_part("t1", false, false, false)])];
    let snapshot = vec![run(
        "run-1",
        Some("t1"),
        SubagentRunMode::Sync,
        SubagentRunStatus::Running,
        now,
    )];
    let base = aggregate_subagents(&entries, &snapshot, now);
    assert!(!base[0].is_navigable());
    let children = vec![child_row("child-1", "run-1", Some("t1"), ms(1000))];
    let out = merge_child_rows(base, &children, |_| Some(PanelStatus::Done));
    assert_eq!(out.len(), 1);
    assert!(out[0].is_navigable());
    assert_eq!(out[0].status, PanelStatus::Done, "child session wins");
    assert_eq!(out[0].agent, "planner");
    assert_eq!(out[0].task, "Plan the panel");
}

/// Child session status mapping: Working/AwaitingInput → Running (Stale
/// past the staleness window), Errored → Error, Idle → Done.
#[test]
fn child_session_status_maps_to_panel() {
    let now = ms(2000);
    assert_eq!(
        child_session_panel_status(SessionStatus::Working, now, now),
        PanelStatus::Running
    );
    assert_eq!(
        child_session_panel_status(SessionStatus::AwaitingInput, now, now),
        PanelStatus::Running
    );
    assert_eq!(
        child_session_panel_status(SessionStatus::Working, now - SUBAGENT_STALE_MS - 1000, now),
        PanelStatus::Stale
    );
    assert_eq!(
        child_session_panel_status(SessionStatus::Errored, now, now),
        PanelStatus::Error
    );
    assert_eq!(
        child_session_panel_status(SessionStatus::Idle, now, now),
        PanelStatus::Done
    );
}

/// Focused-row selection: Down starts at the first row, Up at the last,
/// and movement wraps at both ends.
#[test]
fn next_active_index_wraps_and_starts_at_ends() {
    assert_eq!(next_active_index(None, 1, 3), Some(0));
    assert_eq!(next_active_index(None, -1, 3), Some(2));
    assert_eq!(next_active_index(Some(0), 1, 3), Some(1));
    assert_eq!(next_active_index(Some(2), 1, 3), Some(0), "wraps forward");
    assert_eq!(next_active_index(Some(0), -1, 3), Some(2), "wraps backward");
    assert_eq!(next_active_index(Some(1), 0, 3), Some(1));
    assert_eq!(next_active_index(None, 1, 0), None);
    assert_eq!(next_active_index(Some(0), 1, 0), None);
}

fn chat_row(id: &str, child: Option<cypher_proto::ChildChat>) -> cypher_proto::Chat {
    cypher_proto::Chat {
        id: id.into(),
        created_at: Utc::now(),
        child,
        ..crate::test_fixtures::chat()
    }
}

/// A RUNNING child opens from the inspector: the row is live (its spinner
/// re-renders the popover between press and release), and the click still
/// lands and selects the child's session.
#[gpui::test]
fn clicking_a_running_child_row_opens_its_session(cx: &mut gpui::TestAppContext) {
    let now = Utc::now();
    let state = cx.update(|cx| {
        cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
        cx.new(|_| AppState::new())
    });
    cx.update(|cx| {
        state.update(cx, |s, _| {
            let child = cypher_proto::ChildChat {
                parent_chat_id: "parent".into(),
                parent_run_id: "run-1".into(),
                agent: "planner".into(),
                task: "Plan the panel".into(),
                mode: SubagentRunMode::Sync,
                tool_call_id: Some("planner-1".into()),
                profile: cypher_proto::ChildAgentProfile {
                    system_prompt: "x".into(),
                    tools: vec![],
                    model: None,
                    thinking: None,
                },
            };
            s.apply_chats(vec![
                chat_row("parent", None),
                chat_row("child-1", Some(child)),
            ]);
            let mut r = run(
                "run-1",
                Some("planner-1"),
                SubagentRunMode::Sync,
                SubagentRunStatus::Running,
                now.timestamp_millis(),
            );
            r.child_chat_id = Some("child-1".into());
            s.apply_sessions(vec![
                cypher_proto::Session {
                    chat_id: "parent".into(),
                    status: SessionStatus::Working,
                    updated_at: now,
                    subagents: vec![r],
                    ..crate::test_fixtures::session()
                },
                cypher_proto::Session {
                    chat_id: "child-1".into(),
                    status: SessionStatus::Working,
                    updated_at: now,
                    ..crate::test_fixtures::session()
                },
            ]);
            s.selected_chat = Some("parent".into());
            s.transcript = vec![streaming_entry(vec![subagent_part(
                "planner-1",
                false,
                false,
                false,
            )])];
        });
    });
    let window = cx.open_window(gpui::size(px(1100.0), px(800.0)), |_, cx| {
        SubagentsPanel::new(state.clone(), cx)
    });
    let panel = window.root(cx).expect("panel");
    let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|cx| {
        let opened = opened.clone();
        cx.subscribe(&panel, move |_, event: &SubagentsEvent, _| {
            opened.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    let draw = |visual: &mut gpui::VisualTestContext| {
        visual.update(|w, cx| {
            w.refresh();
            w.draw(cx).clear();
        });
    };
    draw(&mut visual);
    let trigger = visual.debug_bounds("subagents-trigger").expect("trigger");
    visual.simulate_click(trigger.center(), Default::default());
    draw(&mut visual);
    let row = visual.debug_bounds("subagent-open-run-1").expect("row");
    visual.simulate_mouse_move(row.center(), None, Default::default());
    draw(&mut visual);
    visual.simulate_mouse_down(row.center(), gpui::MouseButton::Left, Default::default());
    for _ in 0..3 {
        visual.update(|_, cx| state.update(cx, |_, cx| cx.notify()));
        draw(&mut visual);
    }
    visual.simulate_mouse_up(row.center(), gpui::MouseButton::Left, Default::default());
    draw(&mut visual);
    // The shell opens the child in its own tab; this panel's context
    // keeps its session.
    assert_eq!(
        opened.borrow().as_slice(),
        [SubagentsEvent::OpenChat("child-1".into())]
    );
    let selected = visual.update(|_, cx| state.read(cx).selected_chat.clone());
    assert_eq!(selected.as_deref(), Some("parent"));
}
