use super::*;
use cypher_proto::{FolderEntry, Model, ModelOption, ModelOptionChoice};

#[test]
fn runtime_install_errors_support_old_and_new_engines_without_hiding_other_errors() {
    for message in [
        "harness binary not found: Cypher runtime is not installed",
        "harness binary not found: Cypher Pi Runtime is not installed (/isolated/current/bin/pi)",
    ] {
        assert!(missing_pi_runtime(Some(HarnessId::Pi), message));
        assert!(!missing_pi_runtime(Some(HarnessId::Mock), message));
    }
    assert!(!missing_pi_runtime(
        Some(HarnessId::Pi),
        "Device is offline"
    ));
    assert!(!missing_pi_runtime(
        Some(HarnessId::Pi),
        "model discovery timed out"
    ));
    let remote = runtime_install_guidance("Training server", true);
    assert!(remote.contains("Training server"));
    assert!(remote.contains("Settings → Agents"));
    assert!(remote.contains("Download runtime"));
    assert!(remote.contains("will not install it on the remote device"));
    assert!(!runtime_install_guidance("My Mac", false).contains("remote device"));
}

#[gpui::test]
fn missing_runtime_button_keeps_the_chat_device_and_refresh_clears_errors(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::AppContext;
    use std::{cell::RefCell, rc::Rc};

    struct ErrorView(Entity<Pickers>);
    impl gpui::Render for ErrorView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            div().w(px(500.0)).child(
                self.0
                    .update(cx, |pickers, cx| pickers.runtime_missing_row(&theme, cx)),
            )
        }
    }
    let pickers = cx.update(|cx| {
        cx.set_global(Theme::for_appearance(crate::theme::Appearance::Dark));
        crate::composer::init(cx);
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.local_device_id = Some("local-mac".into());
            state.selected_space = Some("remote-space".into());
            state.spaces.push(
                serde_json::from_value(serde_json::json!({
                    "id": "remote-space", "deviceId": "remote-linux", "path": "/repo",
                    "gitDetected": true, "createdAt": chrono::Utc::now(),
                }))
                .unwrap(),
            );
            state
        });
        cx.new(|cx| Pickers::new(state, cx))
    });
    let emitted = Rc::new(RefCell::new(None));
    let captured = emitted.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&pickers, move |_, event, _| {
            let PickerEvent::OpenAgentSettings { target_device } = event;
            *captured.borrow_mut() = Some(target_device.clone());
        })
    });
    let window = cx.open_window(gpui::size(px(600.0), px(400.0)), |_, _| {
        ErrorView(pickers.clone())
    });
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let button = visual.debug_bounds("model-open-agents").unwrap();
    visual.simulate_click(button.center(), Default::default());
    assert_eq!(emitted.borrow().as_deref(), Some("remote-linux"));

    let generation = pickers.update(cx, |pickers, _| {
        pickers
            .models
            .insert(HarnessId::Pi, Loadable::Error("old install error".into()));
        pickers.harnesses = Loadable::Loading;
        pickers.model_generation
    });
    cx.update(bump_harness_catalog);
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(pickers.read(cx).models.is_empty());
        assert!(matches!(pickers.read(cx).harnesses, Loadable::Idle));
        assert_ne!(pickers.read(cx).model_generation, generation);
    });
}

#[gpui::test]
fn side_chat_picks_stamp_the_fork_row(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;

    // A side-chat fork: one synthetic selected row carrying the parent's
    // config. Model/traits picks must land on it (every send reads it).
    let (state, pickers) = cx.update(|cx| {
        let state = cx.new(|_| {
            let mut state = AppState::new();
            state.local_device_id = Some("local-mac".into());
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "side-1", "deviceId": "local-mac", "archived": false,
                    "createdAt": chrono::Utc::now(),
                    "config": {
                        "harness": "pi", "model": "openai/gpt-5",
                        "reasoning": "high", "sandbox": "workspace-write",
                    },
                }))
                .unwrap(),
            );
            state.selected_chat = Some("side-1".into());
            state
        });
        let pickers = cx.new(|cx| {
            let mut pickers = Pickers::new(state.clone(), cx);
            pickers.set_side_chat();
            pickers
        });
        (state, pickers)
    });
    pickers.update(cx, |pickers, cx| {
        pickers.pick_model("claude-bridge/claude-opus-5".into(), cx);
        pickers.pick_reasoning(ReasoningLevel::Low, cx);
    });
    cx.update(|cx| {
        let config = state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.config.clone())
            .unwrap();
        assert_eq!(config.harness, HarnessId::Pi);
        assert_eq!(config.model.as_deref(), Some("claude-bridge/claude-opus-5"));
        assert_eq!(config.reasoning, Some(ReasoningLevel::Low));
    });
}

fn bare_model(id: &str, label: &str) -> Model {
    Model {
        id: id.into(),
        label: label.into(),
        description: None,
        reasoning_levels: Vec::new(),
        options: Vec::new(),
    }
}

#[test]
fn revalidation_keeps_usable_rows_and_only_reanchors_on_change() {
    let ready = Loadable::Ready(vec![bare_model("openai/gpt-5", "GPT-5")]);
    let grown = Loadable::Ready(vec![
        bare_model("openai/gpt-5", "GPT-5"),
        bare_model("claude-bridge/claude-opus-5", "Claude Opus 5"),
    ]);
    let error = Loadable::Error("relay down".into());
    // A failed refresh must not replace rows the user can still pick.
    assert!(Pickers::keeps_current_rows(Some(&ready), &error));
    // A first load surfaces its error row; a fresh catalog always lands.
    assert!(!Pickers::keeps_current_rows(
        Some(&Loadable::Loading),
        &error
    ));
    assert!(!Pickers::keeps_current_rows(None, &error));
    assert!(!Pickers::keeps_current_rows(Some(&ready), &grown));
    // Same rows confirmed: leave the keyboard highlight where it is.
    assert!(!Pickers::rows_changed(Some(&ready), Some(&ready)));
    // New rows (a host that bundled pi-claude-bridge) or a first load re-anchor.
    assert!(Pickers::rows_changed(Some(&ready), Some(&grown)));
    assert!(Pickers::rows_changed(
        Some(&Loadable::Loading),
        Some(&ready)
    ));
    assert!(Pickers::rows_changed(None, Some(&ready)));
}

#[test]
fn normalize_drops_default_alias_and_folds_orphan_1m_rows() {
    // The shape an OLDER engine serves: a `default` alias row plus
    // 1M-pinned variants with no bare base. Wire labels are kept.
    let models = normalize_model_rows(vec![
        bare_model("default", "Default (recommended)"),
        bare_model("titan[1m]", "Titan (1M context)"),
        bare_model("gpt-x-9[1m]", "GPT X-9"),
        bare_model("nano", "Nano"),
    ]);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["titan", "gpt-x-9", "nano"]
    );
    assert_eq!(models[0].label, "Titan");
    assert_eq!(models[1].label, "GPT X-9");
    // Folded rows pin the Context Window trait to 1M.
    assert!(
        models[0]
            .options
            .iter()
            .any(|o| o.id == "contextWindow" && o.default_choice == "1m")
    );
    assert!(models[2].options.is_empty());

    // A `default`-only list survives (nothing real to prefer).
    let only_default = normalize_model_rows(vec![bare_model("default", "Default")]);
    assert_eq!(only_default.len(), 1);

    // A base-plus-variant pair (already folded by a NEWER engine — the
    // variant never reaches us; belt-and-braces if it does): variant
    // drops, base is untouched.
    let paired = normalize_model_rows(vec![
        bare_model("titan-5", "Titan 5"),
        bare_model("titan-5[1m]", "Titan 5 (1M)"),
    ]);
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0].id, "titan-5");

    // Idempotent over a clean list.
    let clean = vec![bare_model("titan-5", "Titan 5")];
    assert_eq!(normalize_model_rows(clean.clone()), clean);
}

#[test]
fn traits_summary_formats_non_defaults() {
    let model = Model {
        id: "opus".into(),
        label: "Opus".into(),
        description: None,
        reasoning_levels: vec![ReasoningLevel::Medium, ReasoningLevel::High],
        options: vec![
            ModelOption {
                id: "context".into(),
                label: "Context window".into(),
                choices: vec![
                    ModelOptionChoice {
                        id: "standard".into(),
                        label: "Standard".into(),
                    },
                    ModelOptionChoice {
                        id: "1m".into(),
                        label: "1M".into(),
                    },
                ],
                default_choice: "standard".into(),
            },
            ModelOption {
                id: "speed".into(),
                label: "Speed".into(),
                choices: vec![
                    ModelOptionChoice {
                        id: "normal".into(),
                        label: "Normal".into(),
                    },
                    ModelOptionChoice {
                        id: "fast".into(),
                        label: "Fast".into(),
                    },
                ],
                default_choice: "normal".into(),
            },
        ],
    };
    let mut selections = serde_json::Map::new();
    selections.insert("context".into(), serde_json::Value::String("1m".into()));
    selections.insert("speed".into(), serde_json::Value::String("fast".into()));
    assert_eq!(
        traits_summary(Some(&model), Some(ReasoningLevel::High), &selections),
        Some("High · 1M · Fast".to_string())
    );
    // All defaults: the effective choices still read on the trigger.
    assert_eq!(
        traits_summary(Some(&model), None, &serde_json::Map::new()),
        Some("Standard · Normal".to_string())
    );
    // A saved choice the option no longer offers falls back to the default
    // label rather than vanishing or echoing a stale id.
    let mut stale = serde_json::Map::new();
    stale.insert(
        "speed".into(),
        serde_json::Value::String("ludicrous".into()),
    );
    assert_eq!(
        traits_summary(Some(&model), None, &stale),
        Some("Standard · Normal".to_string())
    );
    // Reasoning shows without a model too.
    assert_eq!(
        traits_summary(
            None,
            Some(ReasoningLevel::Ultrathink),
            &serde_json::Map::new()
        ),
        Some("Ultrathink".to_string())
    );
    // Nothing to describe → "Traits" fallback upstream.
    assert_eq!(traits_summary(None, None, &serde_json::Map::new()), None);

    // Customized (bright trigger) only when something departs from its
    // default: default-choice selections and the default reasoning level
    // don't count; stale ids don't either.
    let ladder = model.reasoning_levels.clone();
    assert!(traits_customized(
        Some(&model),
        Some(ReasoningLevel::High),
        &ladder,
        &selections
    ));
    assert!(!traits_customized(
        Some(&model),
        default_reasoning(&ladder),
        &ladder,
        &serde_json::Map::new()
    ));
    let mut defaults = serde_json::Map::new();
    defaults.insert("speed".into(), serde_json::Value::String("normal".into()));
    assert!(!traits_customized(
        Some(&model),
        default_reasoning(&ladder),
        &ladder,
        &defaults
    ));
    assert!(!traits_customized(
        Some(&model),
        default_reasoning(&ladder),
        &ladder,
        &stale
    ));
    assert!(traits_customized(
        Some(&model),
        Some(ReasoningLevel::Medium),
        &ladder,
        &serde_json::Map::new()
    ));
}

#[test]
fn folder_paths_and_breadcrumbs() {
    assert_eq!(parent_path("/home/w/dev"), Some("/home/w".to_string()));
    assert_eq!(parent_path("/home"), Some("/".to_string()));
    assert_eq!(parent_path("/home/"), Some("/".to_string()));
    assert_eq!(parent_path("/"), None);
    assert_eq!(parent_path(""), None);
    assert_eq!(child_path("/home", "w"), "/home/w");
    assert_eq!(child_path("/", "home"), "/home");
    let crumbs = breadcrumbs("/home/w/dev");
    let labels: Vec<&str> = crumbs.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, ["/", "home", "w", "dev"]);
    assert_eq!(crumbs[2].1, "/home/w");
    assert_eq!(breadcrumbs("/").len(), 1);
}

#[test]
fn completion_prefix_lengths() {
    // Case-insensitive; the length indexes into the NAME's bytes.
    assert_eq!(completion_prefix_len("Documents", "doc"), Some(3));
    assert_eq!(&"Documents"[3..], "uments");
    assert_eq!(completion_prefix_len("cypher", "cypher"), Some(6));
    assert_eq!(completion_prefix_len("cypher", ""), Some(0));
    assert_eq!(completion_prefix_len("cypher", "dev"), None);
    // Longer than the name → not a prefix.
    assert_eq!(completion_prefix_len("dev", "devel"), None);
    // Multibyte names slice on a char boundary.
    assert_eq!(completion_prefix_len("héllo", "hé"), Some(3));
    assert_eq!(&"héllo"[3..], "llo");
}

#[test]
fn segment_target_resolution() {
    let names = ["github", "GitHub", "worktree"];
    // Exact casing beats the earlier case-insensitive sibling…
    assert_eq!(segment_target(&names, "GitHub"), Some(1));
    assert_eq!(segment_target(&names, "github"), Some(0));
    // …but with no exact-cased hit, case-insensitive exact still lands.
    assert_eq!(segment_target(&names, "WORKTREE"), Some(2));
    // Unique prefix descends; an ambiguous one keeps the slash honest.
    assert_eq!(segment_target(&names, "work"), Some(2));
    assert_eq!(segment_target(&names, "g"), None);
    assert_eq!(segment_target(&names, "x"), None);
}

#[test]
fn browser_navigation_reducer() {
    let listing = FolderListing {
        path: "/home/w".into(),
        entries: vec![
            FolderEntry {
                name: "notes.txt".into(),
                is_dir: false,
                is_repo: false,
            },
            FolderEntry {
                name: "dev".into(),
                is_dir: true,
                is_repo: false,
            },
            FolderEntry {
                name: "cypher".into(),
                is_dir: true,
                is_repo: true,
            },
        ],
        truncated: false,
    };
    // Files never show as rows.
    assert_eq!(browser_rows(&listing).len(), 2);
    assert_eq!(browser_rows(&listing)[1].name, "cypher");
}

#[test]
fn resolved_chat_config_requires_harness() {
    let mut resolved = ResolvedRunConfig::default();
    assert!(resolved.chat_config().is_none());
    resolved.harness = Some(HarnessId::Mock);
    resolved.model = Some("opus".into());
    resolved.reasoning = Some(ReasoningLevel::High);
    let config = resolved.chat_config().expect("harness set");
    assert_eq!(config.harness, HarnessId::Mock);
    assert_eq!(config.model.as_deref(), Some("opus"));
    assert_eq!(config.sandbox, SandboxLevel::WorkspaceWrite);
}

#[test]
fn default_model_is_first_catalog_row() {
    let models = vec![
        Model {
            id: "flagship".into(),
            label: "Flagship".into(),
            description: None,
            reasoning_levels: vec![],
            options: vec![],
        },
        Model {
            id: "fast".into(),
            label: "Fast".into(),
            description: None,
            reasoning_levels: vec![],
            options: vec![],
        },
    ];
    assert_eq!(default_model(&models).map(|m| &*m.id), Some("flagship"));
    assert!(default_model(&[]).is_none());
}

#[test]
fn default_reasoning_prefers_high_then_medium() {
    use ReasoningLevel::*;
    // Recommended default is High (user-corrected), even on full ladders.
    assert_eq!(
        default_reasoning(&[Low, Medium, High, XHigh, Max, Ultracode, Ultrathink]),
        Some(High)
    );
    assert_eq!(default_reasoning(&[Low, Medium, High, Max]), Some(High));
    // No High: Medium.
    assert_eq!(default_reasoning(&[Minimal, Low, Medium]), Some(Medium));
    // Neither offered: first entry.
    assert_eq!(default_reasoning(&[Minimal, Low]), Some(Minimal));
    // Ladder-less model (Haiku): no reasoning at all.
    assert_eq!(default_reasoning(&[]), None);
}

#[test]
fn clamp_reasoning_keeps_offered_levels_and_heals_foreign_ones() {
    use ReasoningLevel::*;
    let ladder = [Low, Medium, High, Max];
    // A pick the ladder offers survives.
    assert_eq!(clamp_reasoning(Some(Max), &ladder), Some(Max));
    // A remembered level the new model doesn't offer heals to its default.
    assert_eq!(clamp_reasoning(Some(XHigh), &ladder), Some(High));
    // No pick at all resolves to the concrete default too.
    assert_eq!(clamp_reasoning(None, &ladder), Some(High));
    assert_eq!(clamp_reasoning(Some(High), &[]), None);
}

#[test]
fn mock_harness_hidden_unless_alone() {
    let descriptor = |id: HarnessId, name: &str| HarnessDescriptor {
        id,
        name: name.into(),
        supports_steering: true,
        steering_mode: cypher_proto::SteeringMode::StepBoundary,
        reasoning_levels: vec![],
        installed: true,
        enabled: None,
    };
    let mixed = vec![
        descriptor(HarnessId::Mock, "Mock"),
        descriptor(HarnessId::ClaudeCode, "Claude Code"),
        descriptor(HarnessId::Pi, "Pi"),
    ];
    // Production exposes only Pi…
    let visible = visible_harnesses_impl(&mixed, false);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, HarnessId::Pi);
    let only_mock = vec![descriptor(HarnessId::Mock, "Mock")];
    assert_eq!(visible_harnesses_impl(&only_mock, false).len(), 0);
    // …and opted back in by CYPHER_HARNESS=mock (the e2e rig), alongside Pi.
    assert_eq!(visible_harnesses_impl(&mixed, true).len(), 2);
    assert_eq!(visible_harnesses_impl(&mixed, true)[0].id, HarnessId::Mock);
    assert_eq!(visible_harnesses_impl(&mixed, true)[1].id, HarnessId::Pi);
}

#[test]
fn model_provider_id_uses_pi_prefix_and_mock_sentinel() {
    assert_eq!(
        model_provider_id(HarnessId::Pi, "anthropic/claude-sonnet-4"),
        "anthropic"
    );
    assert_eq!(
        model_provider_id(HarnessId::Pi, "openai-codex/gpt-5"),
        "openai-codex"
    );
    assert_eq!(model_provider_id(HarnessId::Pi, "mvp-lab/kimi"), "mvp-lab");
    assert_eq!(model_provider_id(HarnessId::Pi, "unknown/x"), "other");
    assert_eq!(model_provider_id(HarnessId::Mock, "any"), "mock");
    assert_eq!(provider_display_name("anthropic").as_ref(), "Claude");
    assert_eq!(provider_display_name("openai-codex").as_ref(), "ChatGPT");
    assert_eq!(provider_display_name("mvp-lab").as_ref(), "mvp-lab");
}

#[test]
fn offered_harnesses_follow_the_catalog_enabled_flags() {
    let descriptor = |id: HarnessId, name: &str, enabled: Option<bool>| HarnessDescriptor {
        id,
        name: name.into(),
        supports_steering: true,
        steering_mode: cypher_proto::SteeringMode::StepBoundary,
        reasoning_levels: vec![],
        installed: true,
        enabled,
    };
    let catalog = |claude: Option<bool>, codex: Option<bool>, grok: Option<bool>| {
        vec![
            descriptor(HarnessId::Mock, "Mock", Some(false)),
            descriptor(HarnessId::ClaudeCode, "Claude Code", claude),
            descriptor(HarnessId::Codex, "Codex", codex),
            descriptor(HarnessId::Grok, "Grok", grok),
        ]
    };
    // Unsupported harnesses remain hidden even when the catalog predates
    // the enabled flag.
    let offered = offered_harnesses_impl(&catalog(None, None, None), false);
    assert!(offered.is_empty());
    // Non-Pi enablement flags cannot make unsupported harnesses visible.
    let offered = offered_harnesses_impl(&catalog(Some(true), Some(false), Some(true)), false);
    assert!(offered.is_empty());
    // The dev-rig mock opt-in survives the supported-harness filter.
    let offered = offered_harnesses_impl(&catalog(Some(true), Some(false), None), true);
    assert_eq!(
        offered.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![HarnessId::Mock]
    );
    let pi = descriptor(HarnessId::Pi, "Pi", Some(true));
    assert_eq!(
        offered_harnesses_impl(&[pi], false)
            .iter()
            .map(|d| d.id)
            .collect::<Vec<_>>(),
        vec![HarnessId::Pi]
    );
}

#[test]
fn resolve_checkout_plan_pin_is_authoritative_only_for_its_space_on_canvas() {
    let pin = PinnedCheckout {
        space: "s1".into(),
        plan: CheckoutPlan::ReuseWorktree {
            path: "/repo/.worktrees/wt".into(),
            branch: None,
        },
    };
    let derived = CheckoutPlan::CurrentCheckout {
        branch: Some("main".into()),
    };

    // Same space, on the canvas → the pin wins (authoritative without refs).
    assert_eq!(
        Pickers::resolve_checkout_plan(Some(&pin), Some("s1"), true, derived.clone()),
        CheckoutPlan::ReuseWorktree {
            path: "/repo/.worktrees/wt".into(),
            branch: None,
        }
    );
    // A different selected space → the pin never leaks onto it.
    assert_eq!(
        Pickers::resolve_checkout_plan(Some(&pin), Some("s2"), true, derived.clone()),
        derived.clone()
    );
    // Inside a session (not the canvas) → the pin does not apply.
    assert_eq!(
        Pickers::resolve_checkout_plan(Some(&pin), Some("s1"), false, derived.clone()),
        derived.clone()
    );
    // No live row — a "no project" canvas or a dangling space id — → the
    // pin never applies, even though the raw selection id might match.
    assert_eq!(
        Pickers::resolve_checkout_plan(Some(&pin), None, true, derived.clone()),
        derived.clone()
    );
    // No pin → the derived refs-based plan.
    assert_eq!(
        Pickers::resolve_checkout_plan(None, Some("s1"), true, derived.clone()),
        derived.clone()
    );

    // A current-checkout pin (the project plus) resets to branch-less.
    let project_pin = PinnedCheckout {
        space: "s1".into(),
        plan: CheckoutPlan::CurrentCheckout { branch: None },
    };
    assert_eq!(
        Pickers::resolve_checkout_plan(Some(&project_pin), Some("s1"), true, derived.clone()),
        CheckoutPlan::CurrentCheckout { branch: None }
    );
}

#[test]
fn owner_transition_required_is_an_exact_stamp_match() {
    // `target_checkout` synchronizes the owner stamps BEFORE setting the
    // fresh pin; the deferred state observer then re-runs the same rule and
    // must be a no-op (matching stamps → no reset, so the pin survives).
    let some_a = Some("a".to_string());
    let some_b = Some("b".to_string());
    // A real change → transition (the observer would clear a stale pin).
    assert!(Pickers::owner_transition_required(&some_a, &some_b));
    assert!(Pickers::owner_transition_required(&some_a, &None));
    assert!(Pickers::owner_transition_required(&None, &some_b));
    // Matching stamps (what a post-`target_checkout` observer sees) → no-op.
    assert!(!Pickers::owner_transition_required(&some_a, &some_a));
    assert!(!Pickers::owner_transition_required(&None, &None));
}

#[test]
fn no_project_invalidates_pin_not_mere_mask() {
    // The opt-out must CLEAR the pin even though the raw selected-space id
    // is left in place — masking alone (via `pinned_plan`) would let a
    // later re-pick of the same project revive a stale pin.
    assert!(Pickers::no_project_invalidates_pin(true, true));
    // No pin, or no opt-out → nothing to invalidate.
    assert!(!Pickers::no_project_invalidates_pin(false, true));
    assert!(!Pickers::no_project_invalidates_pin(true, false));
    assert!(!Pickers::no_project_invalidates_pin(false, false));
}

#[test]
fn clear_pinned_target_drops_pin_and_its_mirror_only_when_pinned() {
    // A worktree pin mirrors `config.branch` + `config.checkout = Local`
    // (see `target_checkout`); clearing must drop BOTH, else the
    // refs-derived plan reconstructs the same worktree from config+refs
    // after a global New Session or a no-project/reselect.
    let make_pin = || PinnedCheckout {
        space: "s1".into(),
        plan: CheckoutPlan::ReuseWorktree {
            path: "/repo/.worktrees/wt".into(),
            branch: Some("feature".into()),
        },
    };
    // Pinned ReuseWorktree mirror: pin + mirrored branch/checkout clear.
    let mut pinned = Some(make_pin());
    let mut branch = Some("feature".to_string());
    let mut checkout = CheckoutKind::Local;
    Pickers::clear_pinned_target_impl(&mut pinned, &mut branch, &mut checkout);
    assert!(pinned.is_none());
    assert_eq!(branch, None);
    assert_eq!(checkout, CheckoutKind::default());
    // A NewWorktree mirror resets to the default (Local) kind too.
    let mut pinned = Some(make_pin());
    let mut branch = Some("main".to_string());
    let mut checkout = CheckoutKind::NewWorktree;
    Pickers::clear_pinned_target_impl(&mut pinned, &mut branch, &mut checkout);
    assert!(pinned.is_none());
    assert_eq!(branch, None);
    assert_eq!(checkout, CheckoutKind::default());
    // Unpinned: an ordinary/manual draft is preserved untouched.
    let mut pinned = None;
    let mut branch = Some("topic".to_string());
    let mut checkout = CheckoutKind::NewWorktree;
    Pickers::clear_pinned_target_impl(&mut pinned, &mut branch, &mut checkout);
    assert!(pinned.is_none());
    assert_eq!(branch.as_deref(), Some("topic"));
    assert_eq!(checkout, CheckoutKind::NewWorktree);
}

#[test]
fn ref_label_impl_pinned_current_and_reuse_read_bare_branch() {
    // Pinned Current/Reuse display mode: the plan is authoritative over any
    // stale draft `config.checkout` — a same-project hover-add onto a
    // worktree must read as its branch name, never "From …".
    assert_eq!(
        Pickers::ref_label_impl(false, Some("main")),
        SharedString::from("main")
    );
    assert_eq!(
        Pickers::ref_label_impl(false, Some("topic")),
        SharedString::from("topic")
    );
    // A pinned NewWorktree keeps the "From <base>" form.
    assert_eq!(
        Pickers::ref_label_impl(true, Some("main")),
        SharedString::from("From main")
    );
    // Unknown branch (detached worktree / refs never loaded) → placeholder.
    assert_eq!(
        Pickers::ref_label_impl(false, None),
        SharedString::from("Select ref")
    );
    assert_eq!(
        Pickers::ref_label_impl(true, None),
        SharedString::from("Select ref")
    );
}

#[test]
fn checkout_kind_icon_pinned_current_reads_bare_folder() {
    // Pinned "Current checkout" → bare folder (even behind a stale
    // NewWorktree draft); worktree-backed and fresh-worktree targets →
    // folder-with-files.
    assert_eq!(Pickers::checkout_kind_icon(true), crate::icons::FOLDER);
    assert_eq!(
        Pickers::checkout_kind_icon(false),
        crate::icons::FOLDER_WITH_FILES
    );
}
