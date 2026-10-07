//! The boot gate: which top-level phase the app shows.

use super::*;

/// The app gate (zeron's App.tsx phases). Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatePhase {
    /// Booting / probing — splash covers this.
    Loading,
    /// Engine unreachable and embedding failed.
    Failed(String),
    /// Engine up, but signed out — show the sign-in card.
    SignIn,
    /// Signed in but no organization selected — provision or select one.
    OrgGate,
    /// Render the shell.
    Ready,
}

/// Missing scope is treated as synced. Current engines always publish
/// [`WorkspaceScope`] before becoming ready, while old daemons are deliberately
/// kept behind the account gate instead of being mistaken for local runtimes.
pub fn gate_phase(
    connection: &ConnectionStatus,
    workspace_scope: Option<WorkspaceScope>,
    auth: Option<&AuthState>,
) -> GatePhase {
    match connection {
        ConnectionStatus::Connecting => GatePhase::Loading,
        ConnectionStatus::Failed(err) => GatePhase::Failed(err.clone()),
        ConnectionStatus::Ready => match workspace_scope.unwrap_or(WorkspaceScope::Synced) {
            WorkspaceScope::Local | WorkspaceScope::Development => GatePhase::Ready,
            WorkspaceScope::Synced => match auth {
                Some(AuthState::NeedsOrganization { .. }) => GatePhase::OrgGate,
                Some(AuthState::SignedIn { .. }) => GatePhase::Ready,
                Some(AuthState::SignedOut) | None => GatePhase::SignIn,
            },
        },
    }
}

/// Parse an `AuthStatus` frame (`{"state": "signedIn", ...}`). Anything else,
/// including the pre-0.3 engine's `{"_tag": ...}` shape, reads as `None`.
pub fn parse_auth_state(value: &serde_json::Value) -> Option<AuthState> {
    serde_json::from_value::<AuthState>(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserProfile;

    fn user() -> UserProfile {
        UserProfile {
            id: "user-1".into(),
            email: "user@example.com".into(),
            name: None,
            avatar_url: None,
        }
    }

    #[test]
    fn workspace_scope_controls_the_auth_gate() {
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(WorkspaceScope::Local),
                Some(&AuthState::SignedOut),
            ),
            GatePhase::Ready
        );
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::SignedOut),
            ),
            GatePhase::SignIn
        );
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::NeedsOrganization { user: user() }),
            ),
            GatePhase::OrgGate
        );
    }

    #[test]
    fn development_and_local_never_use_the_workos_gate() {
        for scope in [WorkspaceScope::Local, WorkspaceScope::Development] {
            for auth in [
                AuthState::SignedOut,
                AuthState::NeedsOrganization { user: user() },
                AuthState::SignedIn {
                    user: user(),
                    org_id: Some("org-1".into()),
                },
            ] {
                assert_eq!(
                    gate_phase(&ConnectionStatus::Ready, Some(scope), Some(&auth)),
                    GatePhase::Ready
                );
            }
        }
    }

    #[test]
    fn missing_scope_falls_back_to_a_synced_gate() {
        assert_eq!(
            gate_phase(&ConnectionStatus::Ready, None, None),
            GatePhase::SignIn
        );
    }

    #[test]
    fn parse_auth_state_carries_the_avatar_url() {
        let signed_in = serde_json::json!({
            "state": "signedIn",
            "user": {"id": "u1", "email": "a@b.c", "avatarUrl": "https://avatars.example.com/b.png"},
            "orgId": "org_1",
        });
        let parsed = parse_auth_state(&signed_in).expect("SignedIn parses");
        let AuthState::SignedIn { user, org_id } = parsed else {
            panic!("expected SignedIn");
        };
        assert_eq!(org_id.as_deref(), Some("org_1"));
        assert_eq!(
            user.avatar_url.as_deref(),
            Some("https://avatars.example.com/b.png")
        );

        // Frames without an avatar stay readable (None).
        let bare = serde_json::json!({
            "state": "needsOrganization",
            "user": {"id": "u1", "email": "a@b.c"},
        });
        let parsed = parse_auth_state(&bare).expect("NeedsOrganization parses");
        let AuthState::NeedsOrganization { user } = parsed else {
            panic!("expected NeedsOrganization");
        };
        assert_eq!(user.avatar_url, None);

        // Anything else is no auth state, including the retired `_tag` shape.
        for garbage in [
            serde_json::json!({"_tag": "SignedIn", "user": {"id": "u1", "email": "a@b.c"}}),
            serde_json::json!({"state": "wat"}),
            serde_json::json!(42),
        ] {
            assert!(parse_auth_state(&garbage).is_none(), "{garbage}");
        }
    }
}
