use super::*;

fn text_delta(s: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: s.into() }
}

fn reasoning_delta(s: &str) -> AgentEvent {
    AgentEvent::ReasoningDelta { text: s.into() }
}

#[test]
fn reasoning_deltas_merge_and_heartbeats_fold_to_nothing() {
    let mut parts = Vec::new();
    for event in [
        reasoning_delta(""),
        reasoning_delta("Plan"),
        reasoning_delta(""),
        reasoning_delta(" it"),
        text_delta("Answer"),
        reasoning_delta("More"),
    ] {
        fold_event_into_parts(&mut parts, &event);
    }
    assert_eq!(
        parts,
        [
            MessagePart::Reasoning {
                id: "r0".into(),
                text: "Plan it".into(),
            },
            MessagePart::Text {
                id: "t1".into(),
                text: "Answer".into(),
                agent_text: None,
            },
            MessagePart::Reasoning {
                id: "r2".into(),
                text: "More".into(),
            },
        ]
    );
}

#[test]
fn text_deltas_merge_until_broken_by_tool() {
    let mut parts = Vec::new();
    fold_event_into_parts(&mut parts, &text_delta("Hello "));
    fold_event_into_parts(&mut parts, &text_delta("world"));
    assert_eq!(parts.len(), 1);
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolCall {
            id: "tool-1".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        },
    );
    fold_event_into_parts(&mut parts, &text_delta("after"));
    assert_eq!(parts.len(), 3);
    match &parts[2] {
        MessagePart::Text { text, .. } => assert_eq!(text, "after"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn session_started_resets_accumulator() {
    let mut parts = Vec::new();
    fold_event_into_parts(&mut parts, &text_delta("junk"));
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::SessionStarted {
            harness: cypher_proto::HarnessId::Mock,
            model: "m".into(),
            tools: vec![],
            cwd: "/".into(),
            session_id: "s".into(),
            assistant_message_id: "a".into(),
        },
    );
    assert!(parts.is_empty());
}

#[test]
fn tool_call_refresh_is_idempotent() {
    let call = AgentEvent::ToolCall {
        id: "t".into(),
        call: ToolCall::Exec {
            command: "ls".into(),
        },
    };
    let mut once = Vec::new();
    fold_event_into_parts(&mut once, &call);
    let mut twice = once.clone();
    fold_event_into_parts(&mut twice, &call);
    assert_eq!(once, twice);
}

#[test]
fn tool_result_marks_resolution() {
    let mut parts = Vec::new();
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        },
    );
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolResult {
            id: "t".into(),
            is_error: true,
            output: None,
            diff: None,
        },
    );
    match &parts[0] {
        MessagePart::Tool {
            is_error, resolved, ..
        } => {
            assert!(*is_error);
            assert!(*resolved);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn sanitize_strips_heavy_inputs_and_is_idempotent() {
    let call = ToolCall::WriteFile {
        path: "/x".into(),
        content: Some("secret".into()),
    };
    let clean = sanitize_tool_call(&call);
    assert_eq!(
        clean,
        ToolCall::WriteFile {
            path: "/x".into(),
            content: None
        }
    );
    assert_eq!(sanitize_tool_call(&clean), clean);
}

/// The subagent tool keeps ONLY the panel/chip fields (agent, task,
/// async) — cwd/timeout/anything else never enter the doc. Idempotent.
#[test]
fn sanitize_subagent_keeps_only_privacy_safe_fields() {
    let call = ToolCall::Unknown {
        name: "subagent".into(),
        input: Some(serde_json::json!({
            "agent": "planner",
            "task": "Plan the panel\n(step by step)",
            "cwd": "/secret/repo",
            "async": true,
            "timeoutSeconds": 600,
        })),
    };
    let clean = sanitize_tool_call(&call);
    let ToolCall::Unknown { name, input } = &clean else {
        panic!("stays an unknown tool");
    };
    assert_eq!(name, "subagent");
    let args = input
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .expect("kept input");
    assert_eq!(args.len(), 3, "only agent/task/async survive");
    assert_eq!(args["agent"], "planner");
    assert_eq!(args["task"], "Plan the panel\n(step by step)");
    assert_eq!(args["async"], true);
    assert!(args.get("cwd").is_none());
    assert!(args.get("timeoutSeconds").is_none());
    // Idempotent: sanitizing the sanitized call is a no-op.
    assert_eq!(sanitize_tool_call(&clean), clean);
}

/// A task longer than [`SUBAGENT_TASK_MAX_CHARS`] is cut on a Unicode
/// boundary so the stored string stays valid UTF-8.
#[test]
fn sanitize_subagent_truncates_task_on_char_boundary() {
    let long = "é".repeat(SUBAGENT_TASK_MAX_CHARS + 40);
    let call = ToolCall::Unknown {
        name: "subagent".into(),
        input: Some(serde_json::json!({ "agent": "actor", "task": long })),
    };
    let clean = sanitize_tool_call(&call);
    let ToolCall::Unknown { input, .. } = clean else {
        panic!("stays unknown");
    };
    let args = input
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .expect("kept input");
    let task = args["task"].as_str().expect("task string");
    assert_eq!(task.chars().count(), SUBAGENT_TASK_MAX_CHARS);
    assert!(std::str::from_utf8(task.as_bytes()).is_ok(), "valid UTF-8");
}

/// A codemode call keeps its script — and only its script — cut on a
/// char boundary at [`CODEMODE_SCRIPT_MAX_CHARS`]; tool_search keeps its
/// query the same way.
#[test]
fn sanitize_keeps_codemode_script_and_tool_search_query() {
    let call = ToolCall::Unknown {
        name: "codemode".into(),
        input: Some(serde_json::json!({ "code": "return 1", "extra": "dropped" })),
    };
    let clean = sanitize_tool_call(&call);
    assert_eq!(
        clean,
        ToolCall::Unknown {
            name: "codemode".into(),
            input: Some(serde_json::json!({ "code": "return 1" })),
        }
    );
    assert_eq!(sanitize_tool_call(&clean), clean, "idempotent");

    let long = "é".repeat(CODEMODE_SCRIPT_MAX_CHARS + 10);
    let call = ToolCall::Unknown {
        name: "codemode".into(),
        input: Some(serde_json::json!({ "code": long })),
    };
    let ToolCall::Unknown { input, .. } = sanitize_tool_call(&call) else {
        panic!("stays unknown");
    };
    let code = input.as_ref().unwrap()["code"].as_str().unwrap();
    assert_eq!(code.chars().count(), CODEMODE_SCRIPT_MAX_CHARS);

    let call = ToolCall::Unknown {
        name: "tool_search".into(),
        input: Some(serde_json::json!({ "query": "discord", "limit": 8 })),
    };
    assert_eq!(
        sanitize_tool_call(&call),
        ToolCall::Unknown {
            name: "tool_search".into(),
            input: Some(serde_json::json!({ "query": "discord" })),
        }
    );
    // Without the field there is nothing to keep.
    let call = ToolCall::Unknown {
        name: "codemode".into(),
        input: Some(serde_json::json!({ "other": 1 })),
    };
    assert_eq!(
        sanitize_tool_call(&call),
        ToolCall::Unknown {
            name: "codemode".into(),
            input: None
        }
    );
}

/// Ordinary Unknown tools still clear their input wholesale.
#[test]
fn sanitize_other_unknown_still_clears_input() {
    let call = ToolCall::Unknown {
        name: "send_message".into(),
        input: Some(serde_json::json!({ "to": "main", "content": "secret" })),
    };
    let clean = sanitize_tool_call(&call);
    assert_eq!(
        clean,
        ToolCall::Unknown {
            name: "send_message".into(),
            input: None
        }
    );
}

// ── A1 strip (docs/chat2-sync.md) ───────────────────────────────────────

#[test]
fn summarize_keeps_five_lines_without_a_character_cap() {
    assert_eq!(summarize_tool_output(""), None);
    assert_eq!(summarize_tool_output("  \n\t\n"), None);
    assert_eq!(summarize_tool_output("one line"), Some("one line".into()));
    // Small multi-line outputs ride whole — no summary, no "…".
    assert_eq!(
        summarize_tool_output("\n\nfirst real\nsecond"),
        Some("first real\nsecond".into())
    );
    assert_eq!(
        summarize_tool_output("only line\n\n  \n"),
        Some("only line".into())
    );
    // Markdown fences are transport wrapping, never content: stripped
    // even when they'd otherwise be the first line, and a fence-only
    // output is blank.
    assert_eq!(
        summarize_tool_output("```console\nreal content\n```"),
        Some("real content".into())
    );
    assert_eq!(summarize_tool_output("```\n```"), None);
    // Big outputs keep the first five complete lines, with no character
    // cap on any individual line.
    let big = format!(
        "```console\nline one {}\nline two\nline three\nline four\nline five\nline six\n```",
        "x".repeat(400)
    );
    let summary = summarize_tool_output(&big).unwrap();
    assert_eq!(summary.lines().count(), TOOL_OUTPUT_SUMMARY_MAX_LINES);
    assert!(summary.contains(&"x".repeat(400)));
    assert!(!summary.contains("line six"));
}

#[test]
fn diff_stat_counts_line_changes() {
    let stat = diff_stat(&ToolDiff {
        path: "/w/a.rs".into(),
        old_text: Some("a\nb\nc\n".into()),
        new_text: "a\nB\nc\nd\n".into(),
    });
    assert_eq!(stat.path, "/w/a.rs");
    assert_eq!(stat.additions, 2); // B + d
    assert_eq!(stat.deletions, 1); // b
    // New file: every line is an addition.
    let stat = diff_stat(&ToolDiff {
        path: "/w/new.rs".into(),
        old_text: None,
        new_text: "one\ntwo\n".into(),
    });
    assert_eq!((stat.additions, stat.deletions), (2, 0));
}

#[test]
fn fold_strips_output_to_summary_and_diff_to_stats() {
    let mut parts = Vec::new();
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Exec {
                command: "cargo test".into(),
            },
        },
    );
    let full = "running 42 tests\n".repeat(300); // ~5KB, was 4KB inline pre-strip
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolResult {
            id: "t".into(),
            is_error: false,
            output: Some(full.clone()),
            diff: Some(ToolDiff {
                path: "/w/a.rs".into(),
                old_text: Some("a\n".into()),
                new_text: "b\n".into(),
            }),
        },
    );
    match &parts[0] {
        MessagePart::Tool {
            output,
            output_bytes,
            diff,
            diff_stats,
            ..
        } => {
            // The bounded summary is doc-resident and powers the
            // expandable output body; diff text still becomes stats.
            assert_eq!(
                output.as_deref(),
                Some(
                    "running 42 tests\nrunning 42 tests\nrunning 42 tests\nrunning 42 tests\nrunning 42 tests"
                )
            );
            assert_eq!(*output_bytes, None);
            assert!(diff.is_none(), "inline diff text must not enter the doc");
            let stats = diff_stats.as_ref().unwrap();
            assert_eq!(stats.len(), 1);
            assert_eq!((stats[0].additions, stats[0].deletions), (1, 1));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn tool_progress_tails_onto_unresolved_parts_and_resolve_clears() {
    let mut parts = Vec::new();
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Unknown {
                name: "subagent".into(),
                input: None,
            },
        },
    );
    // A long stream keeps the LAST 8 lines (cut head, keep tail).
    let long = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolProgress {
            id: "t".into(),
            output: long.clone(),
        },
    );
    match &parts[0] {
        MessagePart::Tool { progress, .. } => {
            let progress = progress.as_deref().expect("progress column set");
            assert_eq!(progress.lines().count(), 8);
            assert!(
                progress.starts_with("line 12"),
                "tail keeps the LAST lines: {progress}"
            );
            assert!(progress.ends_with("line 19"));
        }
        other => panic!("unexpected {other:?}"),
    }
    // A later tick overwrites the tail.
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolProgress {
            id: "t".into(),
            output: "fresh".into(),
        },
    );
    assert!(matches!(
        &parts[0],
        MessagePart::Tool { progress: Some(p), .. } if p == "fresh"
    ));
    // Resolve clears the transient tail but keeps the bounded result
    // summary for the expandable chip body.
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolResult {
            id: "t".into(),
            is_error: false,
            output: Some("final".into()),
            diff: None,
        },
    );
    match &parts[0] {
        MessagePart::Tool {
            resolved: true,
            progress: None,
            output: Some(output),
            ..
        } => assert_eq!(output, "final"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn translation_updates_only_the_latest_rendered_text_part() {
    let mut parts = Vec::new();
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::TextDelta {
            text: "first".into(),
        },
    );
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Exec {
                command: "true".into(),
            },
        },
    );
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::TextDelta {
            text: "final".into(),
        },
    );
    fold_event_into_parts(&mut parts, &AgentEvent::Translation { text: "译".into() });
    // Only the message's own text is rewritten: an earlier text part
    // belongs to a statement that was already settled before a tool ran.
    assert!(matches!(
        &parts[0],
        MessagePart::Text { text, .. } if text == "first"
    ));
    assert!(matches!(
        &parts[2],
        MessagePart::Text { text, .. } if text == "译"
    ));
}

/// Streaming frames carry the whole rendering, so the fold assigns: the
/// answer grows in place, the last frame wins, and a frame that repeats
/// (a keepalive, or a replay) lands on the same text instead of
/// duplicating it.
#[test]
fn translation_frames_converge_on_the_last_one() {
    let mut parts = Vec::new();
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::TextDelta {
            text: "the answer".into(),
        },
    );
    for frame in ["译", "译文", "译文。", "译文。", "译文。"] {
        fold_event_into_parts(&mut parts, &AgentEvent::Translation { text: frame.into() });
    }
    assert_eq!(parts.len(), 1);
    assert!(matches!(
        &parts[0],
        MessagePart::Text { text, .. } if text == "译文。"
    ));

    // Append mode is rendered by the publisher, so it arrives as one text
    // and folds the same way — re-sending it never appends twice.
    let appended = "the answer\n\n---\n\n译文。";
    for _ in 0..3 {
        fold_event_into_parts(
            &mut parts,
            &AgentEvent::Translation {
                text: appended.into(),
            },
        );
    }
    assert!(matches!(
        &parts[0],
        MessagePart::Text { text, .. } if text == appended
    ));
}

/// A frame with no statement to rewrite is a no-op rather than a new part:
/// it arrives after a boundary cleared the fold, and inventing a text part
/// there would splice the translation into the wrong entry.
#[test]
fn a_translation_with_nothing_to_rewrite_is_dropped() {
    let mut parts = Vec::new();
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::Translation {
            text: "译文".into(),
        },
    );
    assert!(parts.is_empty());
}

#[test]
fn tool_progress_ignores_unknown_and_resolved_ids() {
    let mut parts = Vec::new();
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolCall {
            id: "t".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        },
    );
    // Unknown id: no tool part matches — ignored, parts untouched.
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolProgress {
            id: "ghost".into(),
            output: "noise".into(),
        },
    );
    assert_eq!(parts.len(), 1);
    assert!(matches!(
        &parts[0],
        MessagePart::Tool { progress: None, .. }
    ));
    // Resolve, then a late tick for the same id: ignored.
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolResult {
            id: "t".into(),
            is_error: false,
            output: None,
            diff: None,
        },
    );
    fold_event_into_parts(
        &mut parts,
        &AgentEvent::ToolProgress {
            id: "t".into(),
            output: "late".into(),
        },
    );
    assert!(matches!(
        &parts[0],
        MessagePart::Tool {
            resolved: true,
            progress: None,
            ..
        }
    ));
}

#[test]
fn tail_progress_caps_lines_and_bytes_without_splitting_lines() {
    assert_eq!(tail_progress(""), "");
    assert_eq!(tail_progress("a\nb"), "a\nb");
    // Line cap: last 8.
    let many = (0..30)
        .map(|i| format!("l{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = tail_progress(&many);
    assert_eq!(tail.lines().count(), 8);
    assert!(tail.ends_with("l29"));
    // Byte cap: whole lines only (never a mid-line cut).
    let huge_line = "x".repeat(TOOL_PROGRESS_MAX_BYTES + 100);
    let single = format!("head\n{huge_line}");
    let tail = tail_progress(&single);
    assert_eq!(
        tail, huge_line,
        "the oversized last line rides whole (single line stays readable)"
    );
    // Both caps: last lines within 4KB.
    let wide = (0..200)
        .map(|i| format!("line {i} {}", "y".repeat(60)))
        .collect::<Vec<_>>()
        .join("\n");
    let tail = tail_progress(&wide);
    assert!(
        tail.len() <= TOOL_PROGRESS_MAX_BYTES,
        "{} bytes",
        tail.len()
    );
    assert!(
        tail.starts_with("line "),
        "kept the TAIL, not the head: {tail}"
    );
    // Trailing newline is preserved when budget allows.
    let tail = tail_progress("a\nb\n");
    assert_eq!(tail, "a\nb\n");
}
