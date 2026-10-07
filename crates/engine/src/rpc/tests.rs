use super::*;

/// The UI's Switch/Forget calls send `{id, accountId, harness}` (+ optional
/// `targetDeviceId`); the extra fields must be tolerated, `accountId` wins.
#[test]
fn agent_account_params_accept_ui_shape() {
    let p: AgentAccountParams = parse_params(serde_json::json!({
        "id": "acct-1",
        "accountId": "acct-1",
        "harness": "claude-code",
        "targetDeviceId": "dev-2",
    }))
    .expect("ui param shape");
    assert_eq!(p.account_id, "acct-1");
    assert_eq!(p.harness, HarnessId::ClaudeCode);
}

#[test]
fn local_device_is_not_forwardable() {
    assert!(!forwardable(methods::LOCAL_DEVICE));
    assert!(!forwardable(methods::ENGINE_INFO));
    assert!(!forwardable(methods::ENGINE_READY));
    assert!(forwardable(methods::QUEUE_COMMAND));
    assert!(forwardable(methods::PI_SESSION_MODES));
    assert!(forwardable(methods::RETRY_COMMAND));
    assert!(forwardable(methods::WATCH_DOC_COMMANDS));
    assert!(is_stream_method(methods::WATCH_DOC_COMMANDS));
    assert!(forwardable(methods::SEARCH_FILES));
    assert!(forwardable(methods::SEARCH_GITHUB_ISSUES));
    assert!(forwardable(methods::GET_GITHUB_ISSUE));
    assert!(forwardable(methods::START_GITHUB_LOGIN));
    assert!(forwardable(methods::POLL_GITHUB_LOGIN));
    assert!(forwardable(methods::SIGN_OUT_GITHUB));
    assert!(forwardable(methods::LIST_WORKSPACE_FILES));
    assert!(forwardable(methods::READ_WORKSPACE_FILE));
    assert!(forwardable(methods::WRITE_WORKSPACE_FILE));
    assert!(forwardable(methods::GET_TITLE_MODEL_SETTINGS));
    assert!(forwardable(methods::SET_TITLE_MODEL_SETTINGS));
    assert!(forwardable(methods::GET_WEB_SEARCH_FALLBACK));
    assert!(forwardable(methods::SET_WEB_SEARCH_FALLBACK));
    assert!(forwardable(methods::FETCH_ALL));
}

#[test]
fn tool_file_paths_keep_workspace_activity_only() {
    assert_eq!(
        tool_file_path(&ToolCall::EditFile {
            path: "src/main.rs".into(),
            old_string: None,
            new_string: None,
        }),
        Some("src/main.rs")
    );
    assert_eq!(
        tool_file_path(&ToolCall::Exec {
            command: "cargo test".into(),
        }),
        None
    );
}
