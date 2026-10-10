//! WorkOS exchanges through the Edge: code and email-verification exchange,
//! single-flight refresh, PKCE, and unverified JWT claim decoding.

use super::*;

impl Auth {
    pub(super) async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
    ) -> Result<ExchangeOutcome, EngineError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireUser {
            id: String,
            email: String,
            #[serde(default)]
            first_name: Option<String>,
            #[serde(default)]
            last_name: Option<String>,
            #[serde(default)]
            profile_picture_url: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Exchange {
            user: WireUser,
            access_token: String,
            refresh_token: String,
        }
        let url = format!(
            "{}/auth/exchange",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        let res = self
            .inner
            .http
            .post(&url)
            .json(&serde_json::json!({
                "code": code,
                // RFC 7636 §4.5: the verifier is presented exactly once, at
                // the exchange, to the edge that saw the challenge.
                "codeVerifier": verifier
            }))
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("the edge is unreachable: {e}")))?;
        if res.status() == reqwest::StatusCode::CONFLICT {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Verification {
                code: String,
                pending_authentication_token: String,
                #[serde(default)]
                email: Option<String>,
            }
            let body: Verification = res
                .json()
                .await
                .map_err(|e| EngineError::Other(format!("malformed verification response: {e}")))?;
            if body.code == "email_verification_required"
                && !body.pending_authentication_token.is_empty()
            {
                return Ok(ExchangeOutcome::EmailVerificationRequired {
                    pending_authentication_token: body.pending_authentication_token,
                    email: body.email,
                });
            }
            return Err(EngineError::Other(
                "sign-in returned an invalid email-verification response".into(),
            ));
        }
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "sign-in failed during token exchange ({}) — the code may have expired; start again",
                res.status().as_u16()
            )));
        }
        let body: Exchange = res
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("malformed exchange response: {e}")))?;
        let name = [body.user.first_name, body.user.last_name]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(ExchangeOutcome::Complete(SignInResult {
            user: AuthUser {
                id: body.user.id,
                email: body.user.email,
                name: (!name.is_empty()).then_some(name),
                avatar_url: sanitize_avatar_url(body.user.profile_picture_url),
            },
            access_token: body.access_token,
            refresh_token: body.refresh_token,
        }))
    }
}

impl Auth {
    pub(super) async fn exchange_email_verification(
        &self,
        pending_authentication_token: &str,
        code: &str,
    ) -> Result<SignInResult, EngineError> {
        let url = format!(
            "{}/auth/verify-email",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        let res = self
            .inner
            .http
            .post(&url)
            .json(&serde_json::json!({
                "pendingAuthenticationToken": pending_authentication_token,
                "code": code
            }))
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("the edge is unreachable: {e}")))?;
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "email verification failed ({}) — check the code and try again",
                res.status().as_u16()
            )));
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireUser {
            id: String,
            email: String,
            #[serde(default)]
            first_name: Option<String>,
            #[serde(default)]
            last_name: Option<String>,
            #[serde(default)]
            profile_picture_url: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Exchange {
            user: WireUser,
            access_token: String,
            refresh_token: String,
        }
        let body: Exchange = res
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("malformed verification response: {e}")))?;
        let name = [body.user.first_name, body.user.last_name]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(SignInResult {
            user: AuthUser {
                id: body.user.id,
                email: body.user.email,
                name: (!name.is_empty()).then_some(name),
                avatar_url: sanitize_avatar_url(body.user.profile_picture_url),
            },
            access_token: body.access_token,
            refresh_token: body.refresh_token,
        })
    }
}

impl Auth {
    /// Refresh the session (single-flight). `organization_id` migrates the WorkOS
    /// session to that org; routine refreshes keep the current scope. Returns the new
    /// access token, `None` when signed out / the refresh could not run.
    pub(super) async fn refresh(
        &self,
        organization_id: Option<&str>,
    ) -> Result<Option<String>, EngineError> {
        let _gate = self.inner.refresh_gate.lock().await;
        // Re-check under the gate: the refresh we queued behind may have done the work.
        if organization_id.is_none()
            && let Some(entry) = &*lock(&self.inner.access)
            && entry.remaining() > TOKEN_SLACK
        {
            return Ok(Some(entry.token.clone()));
        }
        let Some(refresh_token) = lock(&self.inner.stored)
            .as_ref()
            .map(|s| s.refresh_token.clone())
        else {
            return Ok(None);
        };
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct RefreshBody<'a> {
            refresh_token: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            organization_id: Option<&'a str>,
        }
        let url = format!(
            "{}/auth/refresh",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        let res = self
            .inner
            .http
            .post(&url)
            .json(&RefreshBody {
                refresh_token: &refresh_token,
                organization_id,
            })
            .send()
            .await;
        let res = match res {
            Ok(res) => res,
            Err(err) => {
                // Network failure is transient: keep the session, but surface the
                // error so the background loop applies its retry delay.
                return Err(EngineError::Other(format!(
                    "could not reach the edge during refresh: {err}"
                )));
            }
        };
        let status = res.status().as_u16();
        if !res.status().is_success() {
            // Permanent rejection requires BOTH an HTTP 401 AND a stable machine
            // `code` that means the refresh token itself is dead (revoked session,
            // deleted user) — it can NEVER succeed again, so degrade to SignedOut
            // and every downstream retry loop quiets down. Everything else — 429
            // rate limits, 502/503 upstream/network failures, or even a 401 whose
            // body carries no recognized code — is transient: the session survives
            // and the caller backs off and retries. This applies to org-switch
            // refreshes too: an `invalid_grant` is a dead refresh token no matter
            // what scope the attempt carried, while a "not a member" rejection
            // surfaces its own (non-permanent) code and stays an error.
            let code = res
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_owned));
            if status == 401 && code.as_deref().is_some_and(is_permanent_refresh_rejection) {
                tracing::warn!(
                    status,
                    code = code.as_deref().unwrap_or(""),
                    "auth: refresh rejected — session revoked; signing out"
                );
                self.sign_out();
                return Ok(None);
            }
            return Err(EngineError::Other(format!("refresh failed ({status})")));
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RefreshUser {
            #[serde(default)]
            profile_picture_url: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Tokens {
            access_token: String,
            refresh_token: String,
            /// Absent = the edge predates avatar sync (preserve the stored
            /// avatar); present = replace/clear from `profilePictureUrl`.
            #[serde(default)]
            user: Option<RefreshUser>,
        }
        let tokens: Tokens = res
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("malformed refresh response: {e}")))?;
        let org_id = jwt_claims(&tokens.access_token).and_then(|c| c.org_id);
        let entry = AccessEntry::fresh(tokens.access_token.clone());
        tracing::info!(ttl_s = entry.ttl.as_secs(), "auth: access token refreshed");
        *lock(&self.inner.access) = Some(entry);
        // `None` (the refresh carried no user) → PRESERVE the stored avatar;
        // `Some(None)` (user present, no/sanitized-away picture) → CLEAR it.
        let avatar_update: Option<Option<String>> = tokens
            .user
            .as_ref()
            .map(|u| sanitize_avatar_url(u.profile_picture_url.clone()));
        let (user, org_changed, avatar_changed) = {
            let mut stored = lock(&self.inner.stored);
            match stored.as_mut() {
                Some(session) => {
                    let changed = session.org_id != org_id;
                    session.refresh_token = tokens.refresh_token;
                    session.org_id = org_id.clone();
                    let mut avatar_changed = false;
                    if let Some(avatar) = avatar_update
                        && session.user.avatar_url != avatar
                    {
                        session.user.avatar_url = avatar;
                        avatar_changed = true;
                    }
                    (session.user.clone(), changed, avatar_changed)
                }
                None => return Ok(None), // signed out mid-refresh
            }
        };
        self.persist(lock(&self.inner.stored).as_ref());
        // Emit when the profile (avatar) or scope changed so already-signed-in
        // surfaces update without a re-login.
        if org_changed || avatar_changed {
            self.inner.state_tx.send_replace(state_for(user, org_id));
        }
        self.inner
            .token_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        Ok(Some(tokens.access_token))
    }
}

pub(super) struct SignInResult {
    pub(super) user: AuthUser,
    pub(super) access_token: String,
    pub(super) refresh_token: String,
}

pub(super) enum ExchangeOutcome {
    Complete(SignInResult),
    EmailVerificationRequired {
        pending_authentication_token: String,
        email: Option<String>,
    },
}

/// A fresh RFC 7636 §4.1 verifier: 43–128 characters of the unreserved URL-safe
/// alphabet. 32 CSPRNG bytes encode to 43 base64url chars (no padding), so the
/// output is both in range and free of any characters needing URL escaping.
pub(super) fn new_pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS CSPRNG must be available");
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 7636 §4.2 S256 challenge: `base64url(sha256(verifier))` without padding.
pub(super) fn pkce_s256_challenge(verifier: &str) -> String {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The stable machine codes that mean the refresh token is permanently dead.
/// The edge only ever emits `invalid_grant` (WorkOS's explicit credential
/// rejection, surfaced on `/auth/refresh`); `refresh_token_invalid` is accepted
/// as a conservative alias. Any other code — or a body with no code at all — is
/// treated as retryable: a transient hiccup must never clear a session.
pub(super) fn is_permanent_refresh_rejection(code: &str) -> bool {
    matches!(code, "invalid_grant" | "refresh_token_invalid")
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct JwtClaims {
    #[serde(default)]
    pub(super) exp: Option<i64>,
    #[serde(default)]
    pub(super) iat: Option<i64>,
    #[serde(default)]
    pub(super) org_id: Option<String>,
}

/// Decode (without verifying — the edge verifies) the JWT payload claims. Total: a
/// malformed token yields `None`, never a panic.
pub(super) fn jwt_claims(token: &str) -> Option<JwtClaims> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
