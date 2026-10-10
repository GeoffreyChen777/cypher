use super::files::tool_file_path;
use super::*;

/// Spot checks that the predicates derived from `cypher_rpc::methods::SPECS`
/// keep their routing meaning.
#[test]
fn routing_predicates_follow_the_method_specs() {
    assert!(!forwardable("NoSuchMethod"));
    assert!(!AuthRpc::handles("NoSuchMethod"));
    assert!(is_stream_method(methods::SUBSCRIBE_TERMINAL));
    assert!(is_stream_method(methods::PI_UPDATE_STATUS));
    // Streams answered only by the local engine are never proxied.
    assert!(!is_stream_method(methods::WATCH_CHATS));
    assert!(!is_stream_method(methods::AUTH_STATUS));
    assert!(!is_stream_method(methods::QUEUE_COMMAND));
    assert!(needs_credential_transport(methods::SAVE_PI_PROVIDER));
    assert!(needs_credential_transport(methods::COMPLETE_MCP_LOGIN));
    assert!(!needs_credential_transport(methods::LIST_MCP_SERVERS));
    assert!(AuthRpc::handles(methods::NOTIFICATION_ACTIVITY));
    assert!(!AuthRpc::handles(methods::ENGINE_INFO));
}

#[test]
fn engine_identity_is_not_forwardable() {
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
