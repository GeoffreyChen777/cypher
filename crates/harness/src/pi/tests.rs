use super::*;

#[test]
fn ui_response_keeps_a_blank_input_answer_distinct_from_cancel() {
    // pi-ask-user's optional comment submitted blank is an answer.
    assert_eq!(
        ui_response_payload("input", Some("")),
        json!({ "value": "" })
    );
    assert_eq!(
        ui_response_payload("editor", Some("")),
        json!({ "value": "" })
    );
    assert_eq!(
        ui_response_payload("input", Some("hi")),
        json!({ "value": "hi" })
    );
    assert_eq!(
        ui_response_payload("input", None),
        json!({ "cancelled": true })
    );
    assert_eq!(
        ui_response_payload("select", Some("A")),
        json!({ "value": "A" })
    );
    assert_eq!(
        ui_response_payload("select", Some("")),
        json!({ "cancelled": true })
    );
    assert_eq!(
        ui_response_payload("select", None),
        json!({ "cancelled": true })
    );
    assert_eq!(
        ui_response_payload("confirm", Some("Confirm")),
        json!({ "confirmed": true })
    );
    assert_eq!(
        ui_response_payload("confirm", None),
        json!({ "confirmed": false })
    );
}

#[test]
fn expected_model_providers_read_newapi_and_claude_bridge() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("extension-settings")).unwrap();
    std::fs::write(
        dir.path().join("extension-settings/provider-newapi.json"),
        r#"{"version":1,"providers":{"mvp":{"baseUrl":"https://x"}}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("settings.json"),
        r#"{"packages":["npm:pi-claude-bridge"]}"#,
    )
    .unwrap();
    let expected = expected_model_providers(dir.path());
    assert!(expected.contains("mvp"), "{expected:?}");
    assert!(expected.contains("claude-bridge"), "{expected:?}");
}

#[test]
fn synthesized_commands_dedup_against_the_probe() {
    let probe = vec![
        SlashCommand {
            name: "compact".into(), // extension wins: no synthesized twin
            description: "Compact the session".into(),
            input_hint: None,
        },
        SlashCommand {
            name: "skill:brave-search".into(),
            description: "Web search via Brave".into(),
            input_hint: None,
        },
    ];
    let commands = synthesize_commands(&probe);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    // Only the missing built-in is appended, at the tail.
    assert_eq!(names, vec!["compact", "skill:brave-search", "export-html"]);
    let tail = &commands[2];
    assert_eq!(
        tail.description,
        "Export the session to an HTML file (pi built-in)"
    );
    assert_eq!(tail.input_hint.as_deref(), Some("output path"));
    // No probe → both synthesized, in order.
    let empty = synthesize_commands(&[]);
    let names: Vec<&str> = empty.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["compact", "export-html"]);
}

#[test]
fn discovered_extension_commands_are_advertised() {
    // Settings → Commands owns hide/show; the harness returns everything.
    let probe = vec![SlashCommand {
        name: "compact-ui-config".into(),
        description: "Interactive compact-ui settings".into(),
        input_hint: None,
    }];
    let commands = synthesize_commands(&probe);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["compact-ui-config", "compact", "export-html"]);
}

#[test]
fn builtin_match_exact_and_prefix_only() {
    // Exact command, with and without an argument.
    assert_eq!(builtin_match("/compact", "compact"), Some(None));
    assert_eq!(
        builtin_match("/compact focus on api", "compact"),
        Some(Some("focus on api"))
    );
    // Whitespace-only rest degrades to no argument.
    assert_eq!(builtin_match("/compact  ", "compact"), Some(None));
    // Ordinary text and lookalikes never match.
    assert_eq!(builtin_match("tell me about /compact", "compact"), None);
    assert_eq!(builtin_match("/compactx", "compact"), None);
    assert_eq!(builtin_match("/compact", "export-html"), None);
    assert_eq!(
        builtin_match("/export-html /tmp/x.html", "export-html"),
        Some(Some("/tmp/x.html"))
    );
}

#[test]
fn context_window_tags_use_m_above_a_million() {
    assert_eq!(context_window_tag(128_000), "128k context");
    assert_eq!(context_window_tag(200_000), "200k context");
    assert_eq!(context_window_tag(1_000_000), "1M context");
    assert_eq!(context_window_tag(1_500_000), "1.5M context");
    assert_eq!(context_window_tag(2_000_000), "2M context");
}

#[test]
fn thinking_levels_map_directly_and_ultra_collapses_to_max() {
    assert_eq!(thinking_level(ReasoningLevel::Minimal), "minimal");
    assert_eq!(thinking_level(ReasoningLevel::Low), "low");
    assert_eq!(thinking_level(ReasoningLevel::Medium), "medium");
    assert_eq!(thinking_level(ReasoningLevel::High), "high");
    assert_eq!(thinking_level(ReasoningLevel::XHigh), "xhigh");
    assert_eq!(thinking_level(ReasoningLevel::Max), "max");
    assert_eq!(thinking_level(ReasoningLevel::Ultra), "max");
    assert_eq!(thinking_level(ReasoningLevel::Ultracode), "max");
    assert_eq!(thinking_level(ReasoningLevel::Ultrathink), "max");
}

#[test]
fn core_tools_map_to_typed_calls() {
    let bash = pi_typed_call("bash", &json!({ "command": "ls -la" }), &[]);
    assert_eq!(
        bash,
        ToolCall::Exec {
            command: "ls -la".into()
        }
    );
    let read = pi_typed_call("read", &json!({ "path": "src/main.rs" }), &[]);
    assert_eq!(
        read,
        ToolCall::ReadFile {
            path: "src/main.rs".into()
        }
    );
    let write = pi_typed_call("write", &json!({ "path": "a.txt", "content": "x" }), &[]);
    assert_eq!(
        write,
        ToolCall::WriteFile {
            path: "a.txt".into(),
            content: None
        }
    );
    let edit = pi_typed_call("edit", &json!({ "path": "a.txt", "edits": [] }), &[]);
    assert_eq!(
        edit,
        ToolCall::EditFile {
            path: "a.txt".into(),
            old_string: None,
            new_string: None,
        }
    );
    let grep = pi_typed_call("grep", &json!({ "pattern": "foo", "path": "src" }), &[]);
    assert_eq!(
        grep,
        ToolCall::Search {
            pattern: "foo".into(),
            path: Some("src".into()),
        }
    );
    let find = pi_typed_call("find", &json!({ "pattern": "*.rs" }), &[]);
    assert_eq!(
        find,
        ToolCall::Glob {
            pattern: "*.rs".into()
        }
    );
    let ls = pi_typed_call("ls", &json!({ "path": "." }), &[]);
    assert_eq!(
        ls,
        ToolCall::Search {
            pattern: String::new(),
            path: Some(".".into()),
        }
    );
    // Extension / unknown tools keep their raw args.
    let unknown = pi_typed_call("myExt", &json!({ "x": 1 }), &[]);
    assert_eq!(
        unknown,
        ToolCall::Unknown {
            name: "myExt".into(),
            input: Some(json!({ "x": 1 })),
        }
    );
    // A codemode script is an extension tool too: its `code` rides as is
    // (the doc's sanitizer decides what persists).
    let script = pi_typed_call("codemode", &json!({ "code": "return 1" }), &[]);
    assert_eq!(
        script,
        ToolCall::Unknown {
            name: "codemode".into(),
            input: Some(json!({ "code": "return 1" })),
        }
    );
    let search = pi_typed_call("web_search", &json!({ "query": "pi 1.0" }), &[]);
    assert_eq!(
        search,
        ToolCall::WebSearch {
            query: "pi 1.0".into()
        }
    );
    // Without a query it is not a search Cypher can name.
    assert!(matches!(
        pi_typed_call("web_search", &json!({}), &[]),
        ToolCall::Unknown { .. }
    ));
}

#[test]
fn mcp_tools_name_their_configured_server() {
    let servers = vec!["mvp-lab-discord".to_string(), "mvp-lab".to_string()];
    let call = pi_typed_call(
        "mcp__mvp_lab_discord__search_messages",
        &json!({ "query": "pi" }),
        &servers,
    );
    assert_eq!(
        call,
        ToolCall::Mcp {
            server: "mvp-lab-discord".into(),
            tool: "search_messages".into(),
            input: Some(json!({ "query": "pi" })),
        }
    );
    assert_eq!(
        mcp_tool_parts("mcp__mvp_lab__read", &servers),
        Some(("mvp-lab".into(), "read".into()))
    );
    // `docs--v2` sanitizes to `docs__v2`, so `docs` also prefixes its
    // tools: the longest configured match wins.
    let servers = vec!["docs".to_string(), "docs--v2".to_string()];
    assert_eq!(
        mcp_tool_parts("mcp__docs__v2__read", &servers),
        Some(("docs--v2".into(), "read".into()))
    );
    // An unknown server splits at the first `__`.
    assert_eq!(
        mcp_tool_parts("mcp__github__create_issue", &servers),
        Some(("github".into(), "create_issue".into()))
    );
    // Not an MCP name, or an incomplete one.
    assert_eq!(mcp_tool_parts("read", &servers), None);
    assert_eq!(mcp_tool_parts("mcp__github", &servers), None);
    assert_eq!(mcp_tool_parts("mcp____tool", &[]), None);
}

#[test]
fn mcp_server_names_read_agent_and_project_configs() {
    let agent = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        agent.path().join("mcp.json"),
        r#"{"mcpServers":{"mvp-lab":{"url":"https://x"},"github":{}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(project.path().join(".pi")).unwrap();
    std::fs::write(
        project.path().join(".pi/mcp.json"),
        r#"{"mcpServers":{"github":{},"local-db":{}}}"#,
    )
    .unwrap();
    let mut names = mcp_server_names(Some(agent.path()), project.path().to_str().unwrap());
    names.sort();
    assert_eq!(names, ["github", "local-db", "mvp-lab"]);
    // Missing files list nothing; they never fail a run.
    assert!(mcp_server_names(None, "/definitely/not/here").is_empty());
}

#[test]
fn codemode_results_drop_their_status_header() {
    let completed = json!({
        "content": [
            { "type": "text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n" },
            { "type": "text", "text": "version 1.0.0.2" },
        ],
    });
    assert_eq!(
        tool_output_text(&without_codemode_header(&completed)).as_deref(),
        Some("version 1.0.0.2")
    );
    // A failure keeps its partial output and the error after the header.
    let failed = json!({
        "content": [
            { "type": "text", "text": "Script failed\nWall time 0.0 seconds\nOutput:\n" },
            { "type": "text", "text": "Script error:\nTypeError: not a function" },
        ],
    });
    assert_eq!(
        tool_output_text(&without_codemode_header(&failed)).as_deref(),
        Some("Script error:\nTypeError: not a function")
    );
    // A script that printed nothing leaves nothing to show.
    let silent = json!({
        "content": [{ "type": "text", "text": "Script completed\nWall time 0.0 seconds\nOutput:\n" }],
    });
    assert_eq!(tool_output_text(&without_codemode_header(&silent)), None);
    // Output that merely starts like a header is the script's own text.
    let own = json!({ "content": [{ "type": "text", "text": "Script completed\nall good" }] });
    assert_eq!(
        tool_output_text(&without_codemode_header(&own)).as_deref(),
        Some("Script completed\nall good")
    );
}

#[test]
fn tool_output_joins_text_blocks_and_caps() {
    let result = json!({
        "content": [
            { "type": "text", "text": "line 1" },
            { "type": "text", "text": "line 2" },
        ],
        "details": {},
    });
    assert_eq!(tool_output_text(&result).as_deref(), Some("line 1\nline 2"));
    // Non-text blocks contribute nothing.
    assert_eq!(
        tool_output_text(&json!({ "content": [{ "type": "image", "data": "x" }] })),
        None
    );
    // The harness 16KB cap applies.
    let big = "x".repeat(OUTPUT_CAP + 100);
    let output = tool_output_text(&json!({ "content": [{ "type": "text", "text": big }] }))
        .expect("capped output");
    assert!(output.len() < OUTPUT_CAP + 32);
    assert!(output.ends_with("… [truncated]"));
}

#[test]
fn models_map_provider_slash_id_with_ladder() {
    let wire = json!({
        "models": [
            {
                "id": "claude-sonnet-4-20250514",
                "name": "Claude Sonnet 4",
                "provider": "anthropic",
                "reasoning": true,
                "contextWindow": 200000,
            },
            {
                "id": "gpt-4o-mini",
                "name": "GPT-4o Mini",
                "provider": "openai",
                "reasoning": false,
                "contextWindow": 128000,
            },
        ]
    });
    let models = models_from_response(&wire);
    assert_eq!(models[0].id, "anthropic/claude-sonnet-4-20250514");
    assert_eq!(models[0].label, "Claude Sonnet 4");
    assert_eq!(
        models[0].description.as_deref(),
        Some("anthropic · 200k context")
    );
    // No `thinkingLevelMap`: pi's base ladder, without the opt-in tiers.
    assert_eq!(
        models[0].reasoning_levels,
        [
            ReasoningLevel::Minimal,
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High
        ]
    );
    assert_eq!(models[1].id, "openai/gpt-4o-mini");
    assert!(models[1].reasoning_levels.is_empty());
    // A provider-less entry still composes an id.
    let bare = json!({ "models": [{ "id": "x", "name": "X" }] });
    assert_eq!(models_from_response(&bare)[0].id, "pi/x");
}

#[test]
fn model_ladder_follows_thinking_level_map() {
    use ReasoningLevel::*;
    let ladder = |map: Value| {
        model_ladder(&json!({ "id": "m", "reasoning": true, "thinkingLevelMap": map }))
    };
    // Live pi shapes (claude-bridge): opt-in tiers, and `null` disabling.
    assert_eq!(
        ladder(json!({ "off": null, "minimal": null, "xhigh": "xhigh", "max": "max" })),
        [Low, Medium, High, XHigh, Max]
    );
    assert_eq!(
        ladder(json!({ "max": "max" })),
        [Minimal, Low, Medium, High, Max]
    );
    assert_eq!(
        ladder(json!({ "minimal": null, "low": null, "medium": null, "high": "high" })),
        [High]
    );
    // Every level disabled, or no reasoning at all: nothing to offer.
    assert!(
        ladder(json!({ "minimal": null, "low": null, "medium": null, "high": null })).is_empty()
    );
    assert!(model_ladder(&json!({ "id": "m", "reasoning": false })).is_empty());
}

#[test]
fn models_fall_back_to_current_state_when_directory_is_empty() {
    let available = json!({ "models": [] });
    let state = json!({
        "model": {
            "id": "grok-4.6",
            "name": "Grok 4.6",
            "provider": "mvp-lab",
            "reasoning": true,
            "contextWindow": 500000,
        },
    });
    let models = models_from_responses(&available, &state);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "mvp-lab/grok-4.6");
    assert_eq!(models[0].label, "Grok 4.6");
    assert_eq!(
        models[0].description.as_deref(),
        Some("mvp-lab · 500k context")
    );
    assert_eq!(models[0].reasoning_levels.len(), 4);

    // A non-empty directory remains authoritative.
    let available = json!({
        "models": [{ "id": "configured", "name": "Configured", "provider": "custom" }]
    });
    let models = models_from_responses(&available, &state);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["custom/configured"]
    );
}

#[test]
fn inline_images_only_image_mimes() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("shot.png");
    let txt = dir.path().join("notes.txt");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(&txt, b"hello").unwrap();
    let images = inline_images(&[png.display().to_string(), txt.display().to_string()])
        .expect("one image inlined");
    let images = images.as_array().expect("array");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["mimeType"], "image/png");
    assert_eq!(images[0]["type"], "image");
    // Nothing image-shaped → None.
    assert!(inline_images(&[txt.display().to_string()]).is_none());
    // Image types providers reject never inline (they'd fail the turn).
    let bmp = dir.path().join("scan.bmp");
    let svg = dir.path().join("logo.svg");
    std::fs::write(&bmp, b"BM").unwrap();
    std::fs::write(&svg, b"<svg/>").unwrap();
    assert!(inline_images(&[bmp.display().to_string(), svg.display().to_string()]).is_none());
}

#[test]
fn subagent_status_key_is_cypher() {
    assert_eq!(SUBAGENTS_STATUS_KEY, "cypher.subagents.v1");
}

#[test]
fn parse_subagent_status_validates_the_v1_snapshot() {
    // Normal snapshot with both a running async run and a settled one.
    let text = json!({
        "version": 1,
        "runs": [
            {
                "runId": "run-1",
                "toolCallId": "t1",
                "agent": "planner",
                "model": "anthropic/claude-sonnet-4",
                "task": "Plan the panel",
                "mode": "async",
                "status": "running",
                "progress": "line 1\nline 2",
                "startedAt": 1000,
                "updatedAt": 2000,
            },
            {
                "runId": "run-2",
                "agent": "reviewer",
                "task": "Review the diff",
                "mode": "message",
                "status": "done",
                "startedAt": 3000,
                "updatedAt": 4000,
                "endedAt": 4000,
            },
        ],
    })
    .to_string();
    let runs = parse_subagent_status(&text).expect("valid snapshot");
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].run_id, "run-1");
    assert_eq!(runs[0].tool_call_id.as_deref(), Some("t1"));
    assert_eq!(runs[0].mode, SubagentRunMode::Async);
    assert_eq!(runs[0].status, SubagentRunStatus::Running);
    assert_eq!(runs[0].progress.as_deref(), Some("line 1\nline 2"));
    assert_eq!(runs[1].mode, SubagentRunMode::Message);
    assert_eq!(runs[1].status, SubagentRunStatus::Done);
    assert_eq!(runs[1].tool_call_id, None);
    assert_eq!(runs[1].ended_at, Some(4000));

    // Blank/missing text is a CLEAR snapshot, not an error.
    assert_eq!(parse_subagent_status(""), Some(vec![]));
    assert_eq!(parse_subagent_status("  \n "), Some(vec![]));
}

#[test]
fn parse_subagent_status_rejects_wrong_versions_and_junk() {
    // Wrong version.
    assert!(parse_subagent_status("{\"version\":2,\"runs\":[]}").is_none());
    assert!(parse_subagent_status("{\"runs\":[]}").is_none());
    // Invalid JSON.
    assert!(parse_subagent_status("not json").is_none());
    assert!(parse_subagent_status("42").is_none());
    // Oversize snapshot (over 64KiB).
    let mut padded = json!({
        "version": 1,
        "runs": [{
            "runId": "r", "agent": "a", "task": "t",
            "mode": "sync", "status": "running",
            "startedAt": 1, "updatedAt": 2,
            "progress": "x",
        }],
    });
    if let Some(obj) = padded.as_object_mut() {
        obj.insert("pad".into(), Value::String("z".repeat(70 * 1024)));
    }
    assert!(parse_subagent_status(&padded.to_string()).is_none());
}

#[test]
fn parse_subagent_status_rejects_oversized_runs_and_bad_enums() {
    let run = |patch: serde_json::Value| {
        let mut r = json!({
            "runId": "r", "agent": "a", "task": "t",
            "mode": "sync", "status": "running",
            "startedAt": 1, "updatedAt": 2,
        });
        if let (Some(obj), Some(patch)) = (r.as_object_mut(), patch.as_object()) {
            for (k, v) in patch {
                obj.insert(k.clone(), v.clone());
            }
        }
        json!({"version": 1, "runs": [r]}).to_string()
    };
    // More than 32 runs.
    let many = json!({
        "version": 1,
        "runs": (0..33).map(|i| json!({
            "runId": format!("r{i}"), "agent": "a", "task": "t",
            "mode": "sync", "status": "running",
            "startedAt": 1, "updatedAt": 2,
        })).collect::<Vec<_>>(),
    });
    assert!(parse_subagent_status(&many.to_string()).is_none());
    // Task over 500 chars.
    assert!(parse_subagent_status(&run(json!({ "task": "x".repeat(501) }))).is_none());
    // Progress over 8 lines.
    assert!(parse_subagent_status(&run(json!({ "progress": "l\n".repeat(9) }))).is_none());
    // Progress over 4KiB.
    assert!(parse_subagent_status(&run(json!({ "progress": "y".repeat(5000) }))).is_none());
    // childChatId over 256 chars is a publisher bug — rejected too.
    assert!(parse_subagent_status(&run(json!({ "childChatId": "c".repeat(257) }))).is_none());
    // A normal childChatId parses through.
    let ok = parse_subagent_status(&run(json!({ "childChatId": "child-1" }))).expect("valid");
    assert_eq!(ok[0].child_chat_id.as_deref(), Some("child-1"));
    // Unknown mode / status enums.
    assert!(parse_subagent_status(&run(json!({ "mode": "blocking" }))).is_none());
    assert!(parse_subagent_status(&run(json!({ "status": "pending" }))).is_none());
    // Missing required fields.
    assert!(parse_subagent_status(&run(json!({ "agent": null }))).is_none());
    assert!(parse_subagent_status(&run(json!({ "startedAt": null }))).is_none());
}

#[test]
fn ui_question_shapes_match_the_bridge() {
    let select = json!({ "title": "Pick", "options": ["A", "B"] });
    // (bridge internals are async; the option extraction shape is what
    // matters — exercise the request-side mapping through the same code.)
    let q = |payload: &Value| -> UserInputQuestion {
        UserInputQuestion {
            id: "u1".into(),
            header: payload
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Agent question")
                .to_owned(),
            question: payload
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Agent question")
                .to_owned(),
            options: payload
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            multi_select: false,
        }
    };
    let question = q(&select);
    assert_eq!(question.options, vec!["A", "B"]);
    assert!(!question.multi_select);
}

#[test]
fn parse_translation_status_accepts_the_v1_frame() {
    let text = parse_translation_status(&json!({ "version": 1, "text": "  译文  " }).to_string())
        .expect("valid frame");
    assert_eq!(text, "译文", "surrounding whitespace is trimmed");
    // A `mode` from the pre-streaming publisher is ignored rather than
    // rejected: the frame now carries the rendering it asked for.
    assert_eq!(
        parse_translation_status(
            &json!({ "version": 1, "text": "hi", "mode": "append" }).to_string()
        )
        .as_deref(),
        Some("hi")
    );
}

#[test]
fn parse_translation_status_rejects_wrong_versions_junk_and_empty_text() {
    for bad in [
        json!({ "version": 2, "text": "hi" }).to_string(),
        json!({ "text": "hi" }).to_string(),
        json!({ "version": 1 }).to_string(),
        json!({ "version": 1, "text": 7 }).to_string(),
        // Whitespace-only would blank the rendered answer.
        json!({ "version": 1, "text": "   " }).to_string(),
        "not json".to_owned(),
        String::new(),
    ] {
        assert!(
            parse_translation_status(&bad).is_none(),
            "must reject {bad}"
        );
    }
}

#[test]
fn parse_input_translation_status_needs_both_sides_of_the_pair() {
    assert_eq!(
        parse_input_translation_status(
            &json!({ "version": 1, "source": " 你好 ", "text": " Hello " }).to_string()
        ),
        Some(("你好".to_owned(), "Hello".to_owned()))
    );
    for bad in [
        json!({ "version": 2, "source": "你好", "text": "Hello" }).to_string(),
        json!({ "version": 1, "text": "Hello" }).to_string(),
        json!({ "version": 1, "source": "你好" }).to_string(),
        json!({ "version": 1, "source": "  ", "text": "Hello" }).to_string(),
        json!({ "version": 1, "source": "你好", "text": "" }).to_string(),
        json!({ "version": 1, "source": "x".repeat(TRANSLATION_STATUS_MAX_BYTES), "text": "y" })
            .to_string(),
        "not json".to_owned(),
    ] {
        assert!(
            parse_input_translation_status(&bad).is_none(),
            "must reject {bad}"
        );
    }
}

#[test]
fn parse_translation_status_bounds_only_on_the_snapshot_byte_cap() {
    // A frame carries the whole rendering, so append mode pays for the
    // original AND its translation. The cap has to clear what the
    // extension's 24k-char source limit can produce, or streaming would
    // stall half way through exactly the longest answers.
    let long = format!("{}\n\n---\n\n{}", "x".repeat(24_000), "译".repeat(24_000));
    let frame = json!({ "version": 1, "text": long }).to_string();
    assert!(
        frame.len() < TRANSLATION_STATUS_MAX_BYTES,
        "{}",
        frame.len()
    );
    assert_eq!(
        parse_translation_status(&frame).map(|text| text.chars().count()),
        Some(48_007)
    );

    let oversized =
        json!({ "version": 1, "text": "x".repeat(TRANSLATION_STATUS_MAX_BYTES) }).to_string();
    assert!(parse_translation_status(&oversized).is_none());
}
