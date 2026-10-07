use super::*;
use cypher_proto::{TodoItem, ToolCall};

#[test]
fn steering_capability_reads_initialize_meta() {
    assert!(steering_supported(&json!({
        "protocolVersion": 1,
        "_meta": { "steering": { "supported": true } },
    })));
    assert!(!steering_supported(&json!({ "protocolVersion": 1 })));
    assert!(!steering_supported(&json!({
        "_meta": { "steering": { "supported": false } },
    })));
}

#[test]
fn config_option_sets_map_model_effort_and_model_options() {
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "claude-sonnet-5",
                "options": [
                    { "value": "claude-sonnet-5", "name": "Sonnet 5" },
                    { "value": "claude-opus-5", "name": "Opus 5" },
                    { "value": "claude-opus-5[1m]", "name": "Opus 5 (1M)" },
                ],
            },
            {
                "id": "effort",
                "name": "Reasoning effort",
                "category": "thought_level",
                "type": "select",
                "currentValue": "high",
                "options": [
                    { "value": "low", "name": "Low" },
                    { "value": "medium", "name": "Medium" },
                    { "value": "high", "name": "High" },
                    { "value": "max", "name": "Max" },
                ],
            },
            {
                "id": "fast_mode",
                "name": "Fast mode",
                "category": "model_config",
                "type": "boolean",
                "currentValue": false,
            },
        ],
    });
    let no_opts = serde_json::Map::new();
    // Model switch + effort preference list; fastMode untouched without a
    // model-option selection.
    assert_eq!(
        config_option_sets(&response, Some("claude-opus-5"), &["medium"], &no_opts),
        vec![
            ("model".to_owned(), json!({ "value": "claude-opus-5" })),
            ("effort".to_owned(), json!({ "value": "medium" })),
        ]
    );
    // Effort preference order: first ADVERTISED candidate wins.
    assert_eq!(
        config_option_sets(&response, None, &["xhigh", "max"], &no_opts),
        vec![("effort".to_owned(), json!({ "value": "max" }))]
    );
    // contextWindow=1m composes the [1m] model id; fastMode=on matches the
    // boolean option across naming styles (fastMode vs fast_mode).
    let mut opts = serde_json::Map::new();
    opts.insert("contextWindow".into(), json!("1m"));
    opts.insert("fastMode".into(), json!("on"));
    assert_eq!(
        config_option_sets(&response, Some("claude-opus-5"), &["high"], &opts),
        vec![
            ("model".to_owned(), json!({ "value": "claude-opus-5[1m]" })),
            (
                "fast_mode".to_owned(),
                json!({ "type": "boolean", "value": true })
            ),
        ]
    );
    // Already-current values and unadvertised models set nothing.
    assert_eq!(
        config_option_sets(&response, Some("claude-sonnet-5"), &["high"], &no_opts),
        Vec::new()
    );
    assert_eq!(
        config_option_sets(&response, Some("gpt-5.6-sol"), &[], &no_opts),
        Vec::new()
    );
    // No configOptions advertised → nothing to set.
    assert_eq!(
        config_option_sets(&json!({"sessionId": "s"}), Some("x"), &["high"], &no_opts),
        Vec::new()
    );
}

#[test]
fn models_prefer_the_model_config_option_over_legacy_available_models() {
    // codex-acp shape: the legacy models state enumerates model × effort,
    // the config options carry base ids + a separate thought_level select.
    let response = json!({
        "sessionId": "s-1",
        "models": {
            "currentModelId": "gpt-5.6-sol low",
            "availableModels": [
                { "modelId": "gpt-5.6-sol low", "name": "GPT-5.6-Sol (low)" },
                { "modelId": "gpt-5.6-sol medium", "name": "GPT-5.6-Sol (medium)" },
                { "modelId": "gpt-5.6-terra low", "name": "GPT-5.6-Terra (low)" },
            ],
        },
        "configOptions": [
            {
                "id": "mode",
                "name": "Mode",
                "category": "mode",
                "type": "select",
                "currentValue": "agent",
                "options": [
                    { "value": "read-only", "name": "Read Only" },
                    { "value": "agent", "name": "Agent" },
                    { "value": "agent-full-access", "name": "Agent (full access)" },
                ],
            },
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "gpt-5.6-sol",
                "options": [
                    { "value": "gpt-5.6-sol", "name": "GPT-5.6-Sol", "description": "Frontier" },
                    { "value": "gpt-5.6-terra", "name": "GPT-5.6-Terra" },
                ],
            },
            {
                "id": "reasoning_effort",
                "name": "Reasoning effort",
                "category": "thought_level",
                "type": "select",
                "currentValue": "medium",
                "options": [
                    { "value": "low", "name": "Low" },
                    { "value": "medium", "name": "Medium" },
                    { "value": "high", "name": "High" },
                ],
            },
            {
                "id": "fast-mode",
                "name": "Fast mode",
                "category": "model_config",
                "type": "select",
                "currentValue": "off",
                "options": [
                    { "value": "off", "name": "Off" },
                    { "value": "on", "name": "On" },
                ],
            },
        ],
    });
    let models = models_from_session(&response, &crate::codex::catalog::static_models());
    // Two base models — never one row per effort variant.
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["gpt-5.6-sol", "gpt-5.6-terra"]
    );
    // Catalog match keeps the curated per-model ladder; wire wins on
    // label/description.
    assert_eq!(models[0].label, "GPT-5.6-Sol");
    assert_eq!(models[0].description.as_deref(), Some("Frontier"));
    assert!(models[0].reasoning_levels.contains(&ReasoningLevel::Ultra));
    // Wire config options become traits; mode/model/thought_level do not.
    assert_eq!(
        models[0]
            .options
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        vec!["fast-mode"]
    );
    assert_eq!(models[0].options[0].default_choice, "off");
}

#[test]
fn model_1m_variants_collapse_into_a_context_window_trait() {
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [
            {
                "id": "model",
                "category": "model",
                "type": "select",
                "currentValue": "claude-sonnet-5",
                "options": [
                    { "value": "claude-sonnet-5", "name": "Sonnet 5" },
                    { "value": "claude-sonnet-5[1m]", "name": "Sonnet 5 (1M)" },
                    // SDK-id hint spelling collapses too.
                    { "value": "claude-opus-4-6", "name": "Opus 4.6" },
                    { "value": "claude-opus-4-6-1m", "name": "Opus 4.6 (1M)" },
                    { "value": "claude-haiku-4-5", "name": "Haiku 4.5" },
                ],
            },
        ],
    });
    let models = models_from_session(&response, &[]);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["claude-sonnet-5", "claude-opus-4-6", "claude-haiku-4-5"]
    );
    assert!(models[0].options.iter().any(|o| o.id == "contextWindow"));
    assert!(models[1].options.iter().any(|o| o.id == "contextWindow"));
    assert!(models[2].options.is_empty());
}

#[test]
fn default_alias_drops_and_orphan_1m_variants_fold_to_their_base() {
    // The real claude adapter advertises a `default` alias row plus
    // `opus[1m]` with NO bare `opus` (the CLI pins the 1M window).
    // Both made the picker read like a settings dump (user report):
    // `default` duplicates a real model, and the orphan 1M variant now
    // presents AS its base model with the Context Window trait pinned
    // to 1M.
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "claude-fable-5[1m]",
            "options": [
                { "value": "default", "name": "Default (recommended)" },
                { "value": "opus[1m]", "name": "Opus (1M context)" },
                { "value": "claude-fable-5[1m]", "name": "Fable 5" },
                { "value": "sonnet", "name": "Sonnet" },
                { "value": "haiku", "name": "Haiku" },
            ],
        }],
    });
    let models = models_from_session(&response, &[]);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["opus", "claude-fable-5", "sonnet", "haiku"]
    );
    // The folded rows keep a de-parenthesized wire name (no catalog
    // here) and carry the 1M-pinned window trait.
    assert_eq!(models[0].label, "Opus");
    let window = models[0].options.iter().find(|o| o.id == "contextWindow");
    assert_eq!(window.map(|o| o.default_choice.as_str()), Some("1m"));
    assert!(
        models[1]
            .options
            .iter()
            .any(|o| o.id == "contextWindow" && o.default_choice == "1m")
    );
    // The bare aliases stay untouched.
    assert!(models[2].options.is_empty());
    assert!(models[3].options.is_empty());
}

#[test]
fn claude_aliases_enrich_from_the_curated_catalog() {
    // Same wire shape, WITH the claude catalog: bare aliases pick up the
    // flagship row's curated label/description/ladder, versioned ids
    // keep their wire name.
    let response = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "default",
            "options": [
                { "value": "default", "name": "Default (recommended)" },
                { "value": "opus[1m]", "name": "Opus (1M context)" },
                { "value": "fable", "name": "Fable" },
                { "value": "sonnet", "name": "Sonnet" },
                { "value": "haiku", "name": "Haiku" },
            ],
        }],
    });
    let models = models_from_session(&response, &crate::claude::catalog::static_models());
    assert_eq!(
        models.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(),
        vec!["Opus 5", "Fable 5", "Sonnet 5", "Haiku 4.5"]
    );
    // The alias rows carry the catalog's per-model ladders.
    assert!(
        models[1]
            .reasoning_levels
            .contains(&ReasoningLevel::Ultracode)
    );
    assert!(models[3].reasoning_levels.is_empty());
    // Versioned ids never fuzzy-match: a foreign id passes through.
    let foreign = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "model", "category": "model", "type": "select",
            "options": [{ "value": "claude-opus-9-mini", "name": "Opus 9 Mini" }],
        }],
    });
    let models = models_from_session(&foreign, &crate::claude::catalog::static_models());
    assert_eq!(models[0].label, "Opus 9 Mini");
}

#[test]
fn models_fall_back_to_legacy_state_with_catalog_options() {
    let response = json!({
        "sessionId": "s-1",
        "models": {
            "availableModels": [
                { "modelId": "gpt-5.6-sol", "name": "GPT-5.6-Sol" },
                { "modelId": "gpt-x", "name": "GPT-X" },
            ],
        },
    });
    let models = models_from_session(&response, &crate::codex::catalog::static_models());
    assert_eq!(models.len(), 2);
    // Catalog-matched id keeps the curated options on the legacy path…
    assert!(models[0].options.iter().any(|o| o.id == "serviceTier"));
    // …unknown ids get none.
    assert!(models[1].options.is_empty());
}

/// `cursor/ask_question` carries several questions at once, each with its
/// own labelled options — the round trip must answer with OPTION IDS,
/// keyed by cursor's wire ids, not the labels cypher showed the user.
#[test]
fn cursor_questions_round_trip_labels_back_to_option_ids() {
    let asked = cursor_questions(&json!({
        "toolCallId": "call_123",
        "title": "Need input",
        "questions": [
            {
                "id": "q1",
                "prompt": "Which mode?",
                "options": [
                    { "id": "agent", "label": "Agent" },
                    { "id": "plan", "label": "Plan" },
                ],
            },
            {
                "id": "q2",
                "prompt": "Which targets?",
                "options": [
                    { "id": "ios", "label": "iOS" },
                    { "id": "mac", "label": "macOS" },
                ],
                "allowMultiple": true,
            },
            // No options: unanswerable, so it never reaches the user.
            { "id": "q3", "prompt": "Anything else?" },
        ],
    }));
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].question.header, "Need input");
    assert_eq!(asked[0].question.question, "Which mode?");
    assert_eq!(asked[0].question.options, vec!["Agent", "Plan"]);
    assert!(!asked[0].question.multi_select);
    assert!(asked[1].question.multi_select);
    // Cursor's repeatable ids never leak into zeron's question ids.
    assert_ne!(asked[0].question.id, "q1");

    let answers = vec![
        UserInputAnswer {
            question_id: asked[0].question.id.clone(),
            labels: vec!["Plan".into()],
        },
        UserInputAnswer {
            question_id: asked[1].question.id.clone(),
            labels: vec!["iOS".into(), "macOS".into()],
        },
    ];
    assert_eq!(
        cursor_answer_outcome(&asked, &answers),
        json!({
            "outcome": {
                "outcome": "answered",
                "answers": [
                    { "questionId": "q1", "selectedOptionIds": ["plan"] },
                    { "questionId": "q2", "selectedOptionIds": ["ios", "mac"] },
                ],
            }
        })
    );
}

/// A dropped resolver (or labels from a stale panel) must unblock the
/// agent as `cancelled` — never a silent pick of some default option.
#[test]
fn cursor_answers_degrade_to_cancelled_not_a_silent_pick() {
    let asked = cursor_questions(&json!({
        "questions": [{
            "id": "q1",
            "prompt": "Ship it?",
            "options": [{ "id": "yes", "label": "Yes" }],
        }],
    }));
    let cancelled = json!({ "outcome": { "outcome": "cancelled" } });
    assert_eq!(cursor_answer_outcome(&asked, &[]), cancelled);
    let stale = vec![UserInputAnswer {
        question_id: asked[0].question.id.clone(),
        labels: vec!["Maybe".into()],
    }];
    assert_eq!(cursor_answer_outcome(&asked, &stale), cancelled);
}

/// Cursor's todo-bearing extension methods render as chips with stable
/// ids, so a stream of updates refreshes one chip instead of stacking.
#[test]
fn cursor_todo_notifications_map_to_stable_chips() {
    let todos = json!({
        "toolCallId": "call_125",
        "merge": true,
        "todos": [
            { "id": "1", "content": "Set up project", "status": "completed" },
            { "id": "2", "content": "Add auth", "status": "in_progress" },
        ],
    });
    let chip_id = |events: &[AgentEvent]| match &events[0] {
        AgentEvent::ToolCall { id, .. } => id.clone(),
        other => panic!("expected a tool call, got {other:?}"),
    };
    let updated = cursor_notification_events(CURSOR_UPDATE_TODOS, &todos);
    assert_eq!(chip_id(&updated), CURSOR_TODOS_CHIP);
    assert_eq!(
        updated[0],
        AgentEvent::ToolCall {
            id: CURSOR_TODOS_CHIP.into(),
            call: ToolCall::Todo {
                items: vec![
                    TodoItem {
                        text: "Set up project".into(),
                        done: true
                    },
                    TodoItem {
                        text: "Add auth".into(),
                        done: false
                    },
                ]
            },
        }
    );
    // A plan lands on its own chip, so the two never overwrite each other.
    let planned = cursor_notification_events(CURSOR_CREATE_PLAN, &todos);
    assert_eq!(chip_id(&planned), CURSOR_PLAN_CHIP);
    // Everything else is noise zeron has nothing to render for.
    assert!(cursor_notification_events(CURSOR_TASK, &todos).is_empty());
    assert!(cursor_notification_events(CURSOR_GENERATE_IMAGE, &todos).is_empty());
    assert!(cursor_notification_events("cursor/unknown", &todos).is_empty());
}

/// The Cursor slot drives `cursor-agent acp` directly — no adapter
/// package — and opts into the parameterized model picker.
#[test]
fn cursor_spec_targets_the_native_acp_server() {
    let spec = cursor_spec();
    assert_eq!(spec.id, HarnessId::Cursor);
    assert_eq!(spec.display_name, "Cursor");
    assert_eq!(spec.executable, "cursor-agent");
    assert_eq!(spec.args, &["acp"]);
    assert!(spec.npm_package.is_none());
    assert_eq!(spec.steering_mode, SteeringMode::TurnBoundary);
    assert!(spec.reasoning_levels.is_empty());
    assert!(
        (spec.models)()
            .iter()
            .all(|m| m.reasoning_levels.is_empty())
    );
    let auto = (spec.models)()
        .into_iter()
        .find(|m| m.id == "auto-smart")
        .expect("static Auto");
    assert!(auto.options.iter().any(|o| o.id == "optimize_for"));
    assert_eq!(
        (spec.effort_values)(Some(ReasoningLevel::Low), None),
        vec!["low"]
    );
    assert_eq!(
        initialize_params(HarnessId::Cursor)["clientCapabilities"]["_meta"]["parameterizedModelPicker"],
        json!(true)
    );
    assert!(
        initialize_params(HarnessId::Grok)["clientCapabilities"]
            .get("_meta")
            .is_none()
    );
}

#[test]
fn cursor_mode_trait_wins_over_no_prompt_fallback() {
    let cursor = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "mode",
            "category": "mode",
            "type": "select",
            "currentValue": "agent",
            "options": [
                { "value": "agent" },
                { "value": "plan" },
                { "value": "ask" },
            ],
        }, {
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "example[reasoning_effort=high]",
            "options": [
                { "value": "example[reasoning_effort=high]" },
                { "value": "example-low[]" },
                { "value": "example-medium[]" },
            ],
        }],
    });
    let mut opts = serde_json::Map::new();
    opts.insert("mode".into(), json!("plan"));
    assert_eq!(
        config_option_sets(&cursor, None, &[], &opts),
        vec![("mode".to_owned(), json!({ "value": "plan" }))]
    );
    // Reasoning Low switches the effort family to the -low sibling.
    assert_eq!(
        config_option_sets(
            &cursor,
            Some("example[reasoning_effort=high]"),
            &["low"],
            &serde_json::Map::new()
        ),
        vec![("model".to_owned(), json!({ "value": "example-low[]" }))]
    );
}

#[test]
fn cursor_optimize_for_sets_on_parameterized_auto() {
    let cursor = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "mode",
            "category": "mode",
            "type": "select",
            "currentValue": "agent",
            "options": [
                { "value": "agent" },
                { "value": "plan" },
                { "value": "ask" },
            ],
        }, {
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": "composer-2.5",
            "options": [
                { "value": "auto-smart" },
                { "value": "composer-2.5" },
            ],
        }, {
            "id": "optimize_for",
            "category": "model_config",
            "type": "select",
            "currentValue": "balanced",
            "options": [
                { "value": "intelligence" },
                { "value": "balanced" },
                { "value": "cost" },
            ],
        }],
    });
    let mut opts = serde_json::Map::new();
    opts.insert("optimize_for".into(), json!("cost"));
    let sets = config_option_sets(
        &cursor,
        Some("auto-smart[optimize_for=balanced]"),
        &[],
        &opts,
    );
    assert!(
        sets.iter()
            .any(|(id, v)| id == "model" && v == &json!({ "value": "auto-smart" })),
        "{sets:?}"
    );
    assert!(
        sets.iter()
            .any(|(id, v)| id == "optimize_for" && v == &json!({ "value": "cost" })),
        "{sets:?}"
    );
}

#[test]
fn codex_exec_approval_options_are_not_a_question() {
    // codex-acp's real exec-approval shape: two allow_always entries (the
    // session allow + a prefix-rule amendment). Must auto-accept.
    let options = vec![
        json!({ "optionId": "allow_once", "name": "Allow Once", "kind": "allow_once" }),
        json!({ "optionId": "allow_always", "name": "Allow for Session", "kind": "allow_always" }),
        json!({ "optionId": "allow_prefix", "name": "Allow Commands Starting With `cargo test`", "kind": "allow_always" }),
        json!({ "optionId": "reject", "name": "Reject", "kind": "reject_once" }),
    ];
    assert!(!is_user_question(&options));
    // AskUserQuestion relays choices without allow/reject kinds.
    let question = vec![
        json!({ "optionId": "a", "name": "Blue" }),
        json!({ "optionId": "b", "name": "Green" }),
    ];
    assert!(is_user_question(&question));
    let mixed = vec![
        json!({ "optionId": "a", "name": "Proceed", "kind": "allow_once" }),
        json!({ "optionId": "b", "name": "Другое", "kind": "other" }),
    ];
    assert!(is_user_question(&mixed));
}

#[test]
fn mode_config_option_prefers_a_no_prompt_mode_per_adapter_naming() {
    let codex = json!({
        "sessionId": "s-1",
        "configOptions": [{
            "id": "mode",
            "category": "mode",
            "type": "select",
            "currentValue": "agent",
            "options": [
                { "value": "read-only" },
                { "value": "agent" },
                { "value": "agent-full-access" },
            ],
        }],
    });
    let no_opts = serde_json::Map::new();
    assert_eq!(
        config_option_sets(&codex, None, &[], &no_opts),
        vec![("mode".to_owned(), json!({ "value": "agent-full-access" }))]
    );
}

#[test]
fn command_scan_finds_nested_advertisements() {
    let init = json!({
        "protocolVersion": 1,
        "agentCapabilities": {
            "_meta": {
                "availableCommands": [
                    { "name": "compact", "description": "Compact the session" },
                ],
            },
        },
    });
    let commands = scan_available_commands(&init);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "compact");
    assert!(scan_available_commands(&json!({ "protocolVersion": 1 })).is_empty());
}
