use super::*;
use crate::{
    DETAIL_MAX, INSTRUCTION_MAX, PLAN_DETAILS_MAX, RESULT_MAX, StepState, WORK_ROW_MAX, WorkState,
};

fn step(id: &str, detail: Option<&str>) -> WorkStep {
    WorkStep {
        id: id.into(),
        title: format!("Step {id}"),
        project: None,
        detail: detail.map(String::from),
        state: StepState::Pending,
        sessions: vec![],
        note: None,
        started_at: None,
        ended_at: None,
        session_choice: Default::default(),
    }
}

fn work(instruction: &str, steps: Vec<WorkStep>) -> BotWork {
    BotWork {
        steered: Vec::new(),
        id: "w1".into(),
        bot_id: "b".into(),
        title: "Ship it".into(),
        instruction: instruction.into(),
        summary: Some("s".repeat(DETAIL_MAX)),
        steps,
        state: WorkState::Proposed,
        chat_id: "c".into(),
        tool_call_id: None,
        thread_chat_id: None,
        created_at: 1,
        started_at: None,
        ended_at: None,
        result: None,
        decision: None,
        stop_requested_at: None,
        notify: false,
        time_limit_minutes: None,
        time_added_minutes: None,
        warned_at: None,
        coordinator_choice: Default::default(),
        body_rev: None,
        result_rev: None,
    }
}

fn row_units(work: &BotWork) -> usize {
    serde_json::to_string(work).unwrap().encode_utf16().count()
}

fn details(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(id, detail)| (id.to_string(), detail.to_string()))
        .collect()
}

// ── rev ─────────────────────────────────────────────────────────────────────

#[test]
fn rev_is_16_hex_of_the_content_without_rev() {
    let body = PlanBody::new("do it".into(), details(&[("1", "a"), ("2", "b")]), 5);
    assert_eq!(body.rev.len(), REV_LEN);
    assert!(
        body.rev
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(body.rev, body.rev(), "the stored rev is the computed one");
    assert!(body.is_intact());

    // The stored rev itself is not hashed.
    let mut renamed = body.clone();
    renamed.rev = "anything".into();
    assert_eq!(renamed.rev(), body.rev);

    // Any change to the content changes it.
    let mut other = body.clone();
    other.details.insert("2".into(), "c".into());
    assert_ne!(other.rev(), body.rev);
    assert!(!other.is_intact(), "a body edited after writing is caught");
    let mut later = body.clone();
    later.at = 6;
    assert_ne!(later.rev(), body.rev);

    // Pinned: the hash of `{"at":5,"details":{"1":"a","2":"b"},"instruction":"do it"}`.
    let canonical = r#"{"at":5,"details":{"1":"a","2":"b"},"instruction":"do it"}"#;
    let digest = Sha256::digest(canonical.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    assert_eq!(body.rev, hex);
}

#[test]
fn rev_does_not_depend_on_key_order() {
    let a = serde_json::json!({ "text": "x", "at": 1, "nested": { "b": 1, "a": [ { "z": 1, "y": 2 } ] } });
    let b: Value = serde_json::from_str(
        r#"{ "nested": { "a": [ { "y": 2, "z": 1 } ], "b": 1 }, "at": 1, "text": "x", "rev": "old" }"#,
    )
    .unwrap();
    assert_eq!(body_rev(&a), body_rev(&b));

    // A result read back from JSON (another field order, its rev) hashes the same.
    let result = PlanResult::new("全部完成\n\"quoted\"".into(), 9);
    let read: PlanResult = serde_json::from_value(serde_json::json!({
        "text": "全部完成\n\"quoted\"", "rev": result.rev, "at": 9,
    }))
    .unwrap();
    assert!(read.is_intact());
    assert_eq!(read, result);
}

#[test]
fn matches_compares_the_stored_rev_only() {
    let body = PlanBody::new("i".into(), BTreeMap::new(), 1);
    assert!(body.matches(Some(&body.rev)));
    assert!(!body.matches(None));
    assert!(!body.matches(Some("0123456789abcdef")));
    let mut blank = body.clone();
    blank.rev.clear();
    assert!(!blank.matches(Some("")), "an empty rev matches nothing");
}

// ── caps: refused, never cut ────────────────────────────────────────────────

#[test]
fn details_over_the_body_cap_are_refused() {
    // No per-step cap: one step may hold all of it.
    let all_in_one = work("go", vec![step("1", Some(&"d".repeat(PLAN_BODY_MAX)))]);
    assert_eq!(all_in_one.check_plan_body(), Ok(()));

    let over = work(
        "go",
        vec![
            step("1", Some(&"d".repeat(20_000))),
            step("2", Some(&"设".repeat(10_001))),
        ],
    );
    let err = over.check_plan_body().unwrap_err();
    assert_eq!(
        err,
        OverCap {
            len: 30_001,
            max: PLAN_BODY_MAX
        }
    );
    assert_eq!(err.over(), 1);
    assert!(err.to_string().contains("30001 characters"));
    let mut refused = over.clone();
    assert_eq!(refused.cut_plan(1), Err(err));
    assert_eq!(refused, over, "a refused plan is left as it was");

    // Whitespace around a detail doesn't count (it is trimmed).
    let padded = work(
        "go",
        vec![step(
            "1",
            Some(&format!("  {}\n", "d".repeat(PLAN_BODY_MAX))),
        )],
    );
    assert_eq!(padded.check_plan_body(), Ok(()));
}

#[test]
fn results_over_the_body_cap_are_refused() {
    assert_eq!(check_result_body(&"r".repeat(RESULT_BODY_MAX)), Ok(()));
    assert_eq!(
        check_result_body(&"r".repeat(RESULT_BODY_MAX + 5)),
        Err(OverCap {
            len: RESULT_BODY_MAX + 5,
            max: RESULT_BODY_MAX
        })
    );
    let mut row = work("go", vec![step("1", None)]);
    let before = row.clone();
    assert!(row.cut_result(&"r".repeat(RESULT_BODY_MAX + 1), 1).is_err());
    assert_eq!(row, before);
}

#[test]
fn host_results_are_cut_and_say_so() {
    let short = "  all done  ";
    assert_eq!(clip_host_result(short), "all done");
    let exact = "r".repeat(RESULT_BODY_MAX);
    assert_eq!(clip_host_result(&exact), exact);
    let long = format!("{} tail", "结".repeat(RESULT_BODY_MAX));
    let cut = clip_host_result(&long);
    assert_eq!(cut.chars().count(), RESULT_BODY_MAX);
    assert!(cut.ends_with(HOST_CUT_MARK));
    assert_eq!(check_result_body(&cut), Ok(()));
}

#[test]
fn the_instruction_is_cut_at_its_body_cap() {
    let mut row = work(
        &"指".repeat(INSTRUCTION_BODY_MAX + 100),
        vec![step("1", Some("d"))],
    );
    let body = row
        .cut_plan(1)
        .unwrap()
        .expect("a long instruction needs a body");
    assert_eq!(body.instruction.chars().count(), INSTRUCTION_BODY_MAX);
    assert!(body.instruction.ends_with('…'));
    assert!(row.instruction.chars().count() <= INSTRUCTION_MAX);
}

// ── previews ────────────────────────────────────────────────────────────────

#[test]
fn a_plan_that_fits_gets_no_body() {
    let mut row = work(
        &"指".repeat(INSTRUCTION_MAX),
        vec![
            step("1", Some(&"d".repeat(PLAN_DETAILS_MAX - 1))),
            step("2", Some("x")),
            step("3", None),
        ],
    );
    let full = row.clone();
    assert_eq!(row.cut_plan(1), Ok(None));
    assert_eq!(row, full, "nothing cut, nothing changed");
    assert_eq!(row.body_rev, None);
}

#[test]
fn a_long_plan_keeps_todays_previews_and_its_full_text_in_the_body() {
    let long = format!("{}\nend", "设".repeat(9_000));
    let mut row = work(
        &format!("  {}  ", "指".repeat(INSTRUCTION_MAX + 10)),
        vec![
            step("1", Some(&long)),
            step("2", Some("  short  ")),
            step("3", None),
        ],
    );
    let body = row.cut_plan(42).unwrap().expect("cut");
    // The body: the whole text, trimmed, of every step that has a detail.
    assert_eq!(body.instruction, "指".repeat(INSTRUCTION_MAX + 10));
    assert_eq!(body.details, details(&[("1", &long), ("2", "short")]));
    assert_eq!(body.at, 42);
    assert!(body.is_intact());
    // The row: today's caps (D7), each cut marked, pointing at the body.
    assert_eq!(row.body_rev.as_deref(), Some(body.rev.as_str()));
    assert_eq!(row.instruction.chars().count(), INSTRUCTION_MAX);
    assert!(row.instruction.ends_with('…'));
    assert!(row.details_len() <= PLAN_DETAILS_MAX);
    assert!(row.steps[0].detail.as_deref().unwrap().ends_with('…'));
    assert_eq!(row.steps[1].detail.as_deref(), Some("short"));
    assert_eq!(row.steps[2].detail, None);
    assert!(row_units(&row) <= WORK_ROW_MAX);

    // Clamping the row again (every edit does) keeps the previews and the rev.
    let again = {
        let mut again = row.clone();
        again.clamp();
        again
    };
    assert_eq!(again, row);
}

#[test]
fn details_that_only_overflow_as_json_still_get_a_body() {
    // Under the character caps, but a control character takes six units in
    // JSON: `fit_row` cuts.
    let escaped = "\u{1}".repeat(PLAN_DETAILS_MAX);
    let mut row = work("go", vec![step("1", Some(&escaped))]);
    let body = row.cut_plan(1).unwrap().expect("fit_row cut the detail");
    assert_eq!(body.details["1"], escaped);
    assert!(row_units(&row) <= WORK_ROW_MAX);
    assert!(row.body_rev.is_some());
}

#[test]
fn a_long_result_gets_a_preview_and_a_body() {
    let mut row = work("go", vec![step("1", Some("d"))]);
    assert_eq!(row.cut_result("  done  ", 3), Ok(None));
    assert_eq!(row.result.as_deref(), Some("done"));
    assert_eq!(row.result_rev, None);

    let long = "果".repeat(RESULT_MAX * 3);
    let result = row.cut_result(&long, 7).unwrap().expect("cut");
    assert_eq!(result.text, long);
    assert_eq!(result.at, 7);
    assert!(result.is_intact());
    assert_eq!(row.result_rev.as_deref(), Some(result.rev.as_str()));
    let preview = row.result.as_deref().unwrap();
    assert!(preview.chars().count() <= RESULT_MAX);
    assert!(preview.ends_with('…'));
    assert!(row_units(&row) <= WORK_ROW_MAX);
}

// ── reading ─────────────────────────────────────────────────────────────────

fn long_plan() -> (BotWork, PlanBody) {
    let mut row = work(
        "do the long thing",
        vec![
            step("1", Some(&"d".repeat(PLAN_DETAILS_MAX + 500))),
            step("2", Some("two")),
        ],
    );
    let body = row.cut_plan(1).unwrap().unwrap();
    (row, body)
}

#[test]
fn plan_text_reads_the_row_when_nothing_was_cut() {
    let row = work("go", vec![step("1", Some("d")), step("2", None)]);
    let text = PlanText::of(&row, None);
    assert_eq!(text.plan, TextSource::Row);
    assert_eq!(text.instruction, "go");
    assert_eq!(text.details, vec![Some("d".to_string()), None]);
    assert_eq!(text.detail(0), Some("d"));
    assert_eq!(text.detail(5), None);
    assert!(!text.is_loading());

    // A body with no rev on the row (body first, row behind) is ignored.
    let stray = WorkBodies {
        plan: Some(PlanBody::new("other".into(), details(&[("1", "other")]), 1)),
        result: None,
    };
    assert_eq!(PlanText::of(&row, Some(&stray)), text);
}

#[test]
fn plan_text_reads_a_matching_body_in_full() {
    let (row, body) = long_plan();
    let mut result_row = row.clone();
    let result = result_row
        .cut_result(&"r".repeat(RESULT_MAX + 1), 2)
        .unwrap()
        .unwrap();
    let bodies = WorkBodies {
        plan: Some(body.clone()),
        result: Some(result.clone()),
    };
    let text = PlanText::of(&result_row, Some(&bodies));
    assert_eq!(text.plan, TextSource::Body);
    assert_eq!(text.instruction, body.instruction);
    assert_eq!(text.detail(0), Some(body.details["1"].as_str()));
    assert_eq!(text.detail(1), Some("two"));
    assert_eq!(text.result_source, TextSource::Body);
    assert_eq!(text.result.as_deref(), Some(result.text.as_str()));
    assert!(!text.is_loading());
}

#[test]
fn plan_text_shows_the_preview_while_the_body_is_missing_or_stale() {
    let (row, body) = long_plan();
    let missing = PlanText::of(&row, None);
    assert_eq!(missing.plan, TextSource::Preview);
    assert_eq!(missing.instruction, row.instruction);
    assert_eq!(missing.details[0], row.steps[0].detail);
    assert_eq!(missing.result_source, TextSource::Row);
    assert!(missing.is_loading());

    assert_eq!(PlanText::of(&row, Some(&WorkBodies::default())), missing);

    // A body of another rev (an older write, or one changed since).
    let mut stale = body.clone();
    stale.rev = "0000000000000000".into();
    let bodies = WorkBodies {
        plan: Some(stale),
        result: None,
    };
    assert_eq!(PlanText::of(&row, Some(&bodies)), missing);

    // The result's own rev is checked on its own.
    let mut done = row.clone();
    done.cut_result(&"r".repeat(RESULT_MAX * 2), 2)
        .unwrap()
        .unwrap();
    let only_plan = WorkBodies {
        plan: Some(body),
        result: None,
    };
    let text = PlanText::of(&done, Some(&only_plan));
    assert_eq!(text.plan, TextSource::Body);
    assert_eq!(text.result_source, TextSource::Preview);
    assert_eq!(text.result, done.result);
    assert!(text.is_loading());
}

#[test]
fn intact_drops_tampered_parts() {
    let (_, body) = long_plan();
    let result = PlanResult::new("r".into(), 9);
    let mut tampered = body.clone();
    tampered.instruction = "rm -rf".into();
    let bodies = WorkBodies {
        plan: Some(tampered),
        result: Some(result.clone()),
    };
    let intact = bodies.intact();
    assert_eq!(intact.plan, None);
    assert_eq!(intact.result, Some(result));
    assert_eq!(bodies.written_at(), Some(9));
    assert_eq!(WorkBodies::default().written_at(), None);
    assert!(WorkBodies::default().is_empty());
}

// ── old rows ────────────────────────────────────────────────────────────────

#[test]
fn rows_without_revs_read_and_write_as_before() {
    let old: BotWork = serde_json::from_value(serde_json::json!({
        "id": "w1", "botId": "b", "title": "Fix", "chatId": "c", "createdAt": 1,
        "instruction": "go", "result": "done",
        "steps": [{ "id": "1", "title": "Do", "detail": "how" }],
    }))
    .unwrap();
    assert_eq!(old.body_rev, None);
    assert_eq!(old.result_rev, None);
    let json = serde_json::to_value(&old).unwrap();
    assert!(json.get("bodyRev").is_none() && json.get("resultRev").is_none());
    let text = PlanText::of(&old, None);
    assert_eq!(
        (text.plan, text.result_source),
        (TextSource::Row, TextSource::Row)
    );
    assert_eq!(text.result.as_deref(), Some("done"));

    let (row, body) = long_plan();
    let json = serde_json::to_value(&row).unwrap();
    assert_eq!(json["bodyRev"], body.rev.as_str());
    let back: BotWork = serde_json::from_value(json).unwrap();
    assert_eq!(back, row);
}

#[test]
fn bodies_have_the_designed_json_shape() {
    let body = PlanBody::new("i".into(), details(&[("1", "d")]), 3);
    assert_eq!(
        serde_json::to_value(&body).unwrap(),
        serde_json::json!({ "rev": body.rev, "instruction": "i", "details": { "1": "d" }, "at": 3 })
    );
    let result = PlanResult::new("t".into(), 4);
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::json!({ "rev": result.rev, "text": "t", "at": 4 })
    );
    // A malformed or partial entry still reads.
    let partial: WorkBodies = serde_json::from_value(serde_json::json!({ "result": {} })).unwrap();
    assert_eq!(partial.plan, None);
    assert_eq!(partial.result.unwrap().rev, "");
}
