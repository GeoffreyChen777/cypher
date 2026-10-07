use super::*;

#[test]
fn base64url_round_trips_jwt_payload() {
    let payload = br#"{"exp":100,"iat":40,"org_id":"org_1"}"#;
    // Standard base64url without padding (as JWTs use).
    let encoded = {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in payload.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(n >> 6) as usize & 63] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[n as usize & 63] as char);
            }
        }
        out
    };
    assert_eq!(
        base64url_decode(&encoded).as_deref(),
        Some(payload.as_slice())
    );
    let token = format!("h.{encoded}.sig");
    let claims = jwt_claims(&token).expect("claims decode");
    assert_eq!(claims.exp, Some(100));
    assert_eq!(claims.iat, Some(40));
    assert_eq!(claims.org_id.as_deref(), Some("org_1"));
}

#[test]
fn url_coding_round_trips() {
    let raw = "http://127.0.0.1:1234/callback?x=a b&y=%";
    assert_eq!(url_decode(&url_encode(raw)), raw);
    assert_eq!(url_encode("a b"), "a%20b");
}

// -- Avatar URL sanitization (every ingress) -----------------------

#[test]
fn sanitize_avatar_url_accepts_only_safe_https_urls() {
    let good = "https://avatars.example.com/a.png";
    assert_eq!(
        sanitize_avatar_url(Some(good.into())).as_deref(),
        Some(good)
    );
    // Query strings and paths are fine.
    assert_eq!(
        sanitize_avatar_url(Some("https://avatars.example.com/a.png?v=1&s=2".into())).as_deref(),
        Some("https://avatars.example.com/a.png?v=1&s=2")
    );
    // Exactly 2048 chars is allowed.
    let at_cap = format!("https://h/{}", "a".repeat(2048 - "https://h/".len()));
    assert!(sanitize_avatar_url(Some(at_cap)).is_some());
}

#[test]
fn sanitize_avatar_url_rejects_non_https_hostless_and_oversized() {
    for bad in [
        None,
        Some(String::new()),
        // Non-HTTPS schemes (including file/javascript interpretations).
        Some("http://avatars.example.com/a.png".into()),
        Some("file:///etc/passwd".into()),
        Some("javascript:alert(1)".into()),
        Some("ftp://x/a.png".into()),
        // Hostless.
        Some("https://".into()),
        Some("https:///a.png".into()),
        Some("https://?x=1".into()),
        // Embedded credentials (userinfo) must not ride along.
        Some("https://user@host/a.png".into()),
        Some("https://user:pass@host/a.png".into()),
        // Malformed ports / hosts that a hand-rolled prefix check would
        // accept but real URL parsing rejects.
        Some("https://host:99999/a.png".into()),
        Some("https://host:abc/a.png".into()),
        Some("https://:443/a.png".into()),
        Some("https://host:443:444/a.png".into()),
        // Whitespace / control characters (would break URI parsing).
        Some("https://host/a b.png".into()),
        Some("https://host/\n".into()),
        // Oversized: 2048 chars is the cap.
        Some(format!("https://host/{}", "a".repeat(2048))),
    ] {
        assert_eq!(
            sanitize_avatar_url(bad.clone()),
            None,
            "{bad:?} must be rejected"
        );
    }
}

// -- PKCE (RFC 7636) ------------------------------------------------

/// Parse `k=v&k2=v2` query params with the production url-decoder.
fn query_params(url: &str) -> HashMap<String, String> {
    url.split_once('?')
        .map(|(_, q)| q)
        .unwrap_or_default()
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), url_decode(v)))
        .collect()
}

#[test]
fn pkce_verifier_is_random_url_safe_and_in_range() {
    let a = new_pkce_verifier();
    let b = new_pkce_verifier();
    for v in [&a, &b] {
        assert!(
            (43..=128).contains(&v.len()),
            "verifier must be 43-128 chars, got {}",
            v.len()
        );
        assert!(
            v.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~')),
            "verifier must be URL-safe unreserved: {v}"
        );
    }
    // Two draws must never collide: a CSPRNG, not a counter/timestamp.
    assert_ne!(a, b);
}

#[test]
fn pkce_s256_challenge_matches_independent_sha256() {
    use sha2::Digest as _;
    let verifier = new_pkce_verifier();
    let challenge = pkce_s256_challenge(&verifier);
    // Independent recomputation, exactly as the WorkOS authorize endpoint
    // will: base64url(sha256(verifier)), no padding.
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    assert_eq!(challenge, URL_SAFE_NO_PAD.encode(digest));
    assert_eq!(challenge.len(), 43);
}

#[test]
fn authorize_url_carries_pkce_challenge_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = AuthConfig::new("http://edge.test", dir.path());
    config.workos_client_id = Some("client_test".into());
    let auth = Auth::new(config);
    let url = auth.start_headless_sign_in();
    assert!(url.starts_with("https://api.workos.com/user_management/authorize?"));
    let params = query_params(&url);
    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("client_test")
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let state = params.get("state").expect("state present");
    let challenge = params.get("code_challenge").expect("challenge present");
    // The URL challenge is exactly the S256 challenge of the verifier
    // bound to that same state (proving they travel together).
    let (generation, verifier) = auth.take_pending(state).expect("pending state exists");
    assert_eq!(challenge, &pkce_s256_challenge(&verifier));
    let _ = generation;
}

#[test]
fn pending_state_is_consumed_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = AuthConfig::new("http://edge.test", dir.path());
    config.workos_client_id = Some("client_test".into());
    let auth = Auth::new(config);
    let url = auth.start_headless_sign_in();
    let state = query_params(&url).remove("state").expect("state present");

    let first = auth.take_pending(&state);
    assert!(first.is_some(), "first take yields state+verifier");
    // The same state is dead now — a replayed callback can never exchange.
    assert_eq!(auth.take_pending(&state), None);
    // Unknown states were never pending.
    assert_eq!(auth.take_pending("state-that-never-existed"), None);
}

#[test]
fn expired_pending_state_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = AuthConfig::new("http://edge.test", dir.path());
    config.workos_client_id = Some("client_test".into());
    let auth = Auth::new(config);
    // Backdate a pending attempt past the TTL (the test module sees the
    // private lifecycle, so it can simulate the clock).
    let mut sign_in = lock(&auth.inner.sign_in);
    sign_in.pending.insert(
        "stale-state".into(),
        PendingSignIn {
            verifier: new_pkce_verifier(),
            at: Instant::now() - SIGN_IN_TTL - Duration::from_secs(1),
        },
    );
    drop(sign_in);
    assert_eq!(auth.take_pending("stale-state"), None);
}

#[test]
fn sign_out_erases_pending_verifiers() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = AuthConfig::new("http://edge.test", dir.path());
    config.workos_client_id = Some("client_test".into());
    let auth = Auth::new(config);
    let url = auth.start_headless_sign_in();
    let state = query_params(&url).remove("state").expect("state present");
    auth.sign_out();
    // Cancellation fenced the attempt: state AND verifier are gone, so a
    // late callback cannot exchange.
    assert_eq!(auth.take_pending(&state), None);
    assert!(lock(&auth.inner.sign_in).pending.is_empty());
}

// -- Wire-level PKCE (mock edge, real loopback listener) -------------

/// A one-shot HTTP "edge": captures the first request body and answers
/// with the given JSON exchange payload. Returns `(base_url, body_rx)`.
async fn mock_edge(json_body: &'static str) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        let head_end = loop {
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0, "edge: client hung up before the request body");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]);
        let content_length: usize = head
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                (k.trim().eq_ignore_ascii_case("content-length"))
                    .then(|| v.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < head_end + content_length {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "edge: client hung up mid-body");
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buf[head_end..head_end + content_length]).into_owned();
        let _ = tx.send(body);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json_body}",
            json_body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

/// `Auth` in WorkOS mode against a mock edge, with a scratch data dir.
fn workos_auth(edge_url: &str) -> (Auth, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = AuthConfig::new(edge_url, dir.path());
    config.workos_client_id = Some("client_test".into());
    (Auth::new(config), dir)
}

/// `workos_auth` plus a persisted WorkOS session (refresh token + user +
/// org), the state a signed-in device has on disk.
fn workos_auth_with_session(edge_url: &str) -> (Auth, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let session = StoredSession {
        refresh_token: "rt-1".into(),
        user: AuthUser {
            id: "u1".into(),
            email: "a@b.c".into(),
            name: None,
            avatar_url: None,
        },
        org_id: Some("org_1".into()),
    };
    std::fs::write(
        dir.path().join("session.json"),
        serde_json::to_vec(&session).unwrap(),
    )
    .unwrap();
    let auth = Auth::new(config_with_edge(edge_url, dir.path()));
    assert!(auth.state().is_signed_in());
    (auth, dir)
}

fn config_with_edge(edge_url: &str, data_dir: &std::path::Path) -> AuthConfig {
    let mut config = AuthConfig::new(edge_url, data_dir);
    config.workos_client_id = Some("client_test".into());
    config
}

/// A one-shot HTTP "edge" answering with an arbitrary status line + body
/// (drains the request first so the client's write completes).
async fn mock_edge_status(status: &'static str, body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        let head_end = loop {
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]);
        let content_length: usize = head
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                (k.trim().eq_ignore_ascii_case("content-length"))
                    .then(|| v.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < head_end + content_length {
            let Ok(n) = stream.read(&mut chunk).await else {
                return;
            };
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    format!("http://127.0.0.1:{port}")
}

/// A one-shot HTTP "edge" that accepts the connection and closes without
/// answering — a transport failure (dropped connection).
async fn mock_edge_dropped() -> String {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    });
    format!("http://127.0.0.1:{port}")
}

const EXCHANGE_OK: &str = r#"{"user":{"id":"u1","email":"a@b.c","firstName":"Ann","lastName":"X","profilePictureUrl":"https://avatars.example/a.png"},"accessToken":"at","refreshToken":"rt"}"#;

/// Drive the headed flow end-to-end: authorize URL carries the S256
/// challenge; the loopback callback exchanges the code and the captured
/// body proves the verifier for that exact challenge is presented exactly
/// once — a replayed callback is rejected 400.
#[tokio::test]
async fn loopback_exchange_presents_verifier_for_its_challenge_once() {
    let (edge, body_rx) = mock_edge(EXCHANGE_OK).await;
    let (auth, _dir) = workos_auth(&edge);

    let url = auth.start_sign_in().await.expect("sign-in URL");
    let params = query_params(&url);
    let state = params.get("state").expect("state").clone();
    let challenge = params.get("code_challenge").expect("challenge").clone();
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let callback = params
        .get("redirect_uri")
        .expect("loopback redirect")
        .clone();
    let port: u16 = callback
        .split('/')
        .nth(2)
        .and_then(|host| host.rsplit_once(':').and_then(|(_, p)| p.parse().ok()))
        .expect("loopback port");

    async fn callback_get(port: u16, code: &str, state: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(
                format!(
                    "GET /callback?code={code}&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut resp = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            resp.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&resp).into_owned()
    }

    let first = callback_get(port, "auth-code-1", &state).await;
    assert!(first.contains("200 OK"), "first callback: {first}");

    let body = tokio::time::timeout(Duration::from_secs(5), body_rx)
        .await
        .expect("exchange reached the edge")
        .expect("edge captured body");
    let sent: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(sent["code"], "auth-code-1");
    let verifier = sent["codeVerifier"].as_str().expect("codeVerifier present");
    // The verifier that left this device is the one whose S256 hash was
    // published in the authorize URL — nothing else could pass WorkOS.
    assert_eq!(challenge, pkce_s256_challenge(verifier));

    // Replay the same callback: the state+verifier pair was consumed.
    let second = callback_get(port, "auth-code-1", &state).await;
    assert!(second.contains("400 Bad Request"), "replay: {second}");
}

/// The headless paste-code path: `complete_sign_in` consumes the pending
/// state+verifier, exchanges with `codeVerifier`, and a second paste of
/// the same code is rejected without touching the edge again.
#[tokio::test]
async fn headless_exchange_consumes_verifier_exactly_once() {
    let (edge, body_rx) = mock_edge(EXCHANGE_OK).await;
    let (auth, _dir) = workos_auth(&edge);

    let url = auth.start_headless_sign_in();
    let params = query_params(&url);
    let state = params.get("state").expect("state").clone();
    let challenge = params.get("code_challenge").expect("challenge").clone();
    let pasted = format!("{state}.paste-code-1");
    auth.complete_sign_in(&pasted)
        .await
        .expect("first paste signs in");
    // The GitHub/WorkOS profile picture rides the exchange and lands in
    // the signed-in user profile (for the sidebar avatar).
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        Some("https://avatars.example/a.png")
    );

    let body = tokio::time::timeout(Duration::from_secs(5), body_rx)
        .await
        .expect("exchange reached the edge")
        .expect("edge captured body");
    let sent: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(sent["code"], "paste-code-1");
    let verifier = sent["codeVerifier"].as_str().expect("codeVerifier present");
    assert_eq!(challenge, pkce_s256_challenge(verifier));

    // The verifier was single-use: a replayed paste must fail without a
    // second edge round trip (take_pending already returned None).
    assert!(auth.complete_sign_in(&pasted).await.is_err());
}

#[test]
fn auth_state_serializes_as_proto_shape() {
    let user = AuthUser {
        id: "u1".into(),
        email: "u@x".into(),
        name: None,
        avatar_url: None,
    };
    let signed_in = AuthState::SignedIn {
        user: user.clone(),
        org_id: Some("org_1".into()),
    };
    let value = serde_json::to_value(&signed_in).expect("json");
    assert_eq!(
        value,
        serde_json::json!({
            "state": "signedIn",
            "user": {"id": "u1", "email": "u@x", "name": null},
            "orgId": "org_1",
        })
    );
    // The proto type itself round-trips the emitted value.
    let parsed: cypher_proto::AuthState = serde_json::from_value(value).expect("proto parse");
    assert!(matches!(parsed, cypher_proto::AuthState::SignedIn { .. }));
    assert_eq!(
        serde_json::to_value(AuthState::SignedOut).expect("json"),
        serde_json::json!({"state": "signedOut"})
    );
    assert_eq!(
        serde_json::to_value(AuthState::NeedsOrganization { user }).expect("json"),
        serde_json::json!({
            "state": "needsOrganization",
            "user": {"id": "u1", "email": "u@x", "name": null},
        })
    );
}

// -- Refresh error semantics (Phase C) --------------------------------
//
// A transient WorkOS/edge failure must NEVER revoke a signed-in device:
// only an explicit permanent credential rejection (401 + `invalid_grant`)
// clears the session. 429, 5xx, malformed bodies, and dropped connections
// all preserve the stored session and surface an error to retry.

/// The stored session still on disk, for asserting preservation.
fn stored_session(dir: &tempfile::TempDir) -> StoredSession {
    let bytes = std::fs::read(dir.path().join("session.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn refresh_invalid_grant_signs_out_and_removes_session() {
    let edge = mock_edge_status(
        "401 Unauthorized",
        r#"{"error":"invalid_grant","code":"invalid_grant","retryable":false}"#,
    )
    .await;
    let (auth, dir) = workos_auth_with_session(&edge);

    let result = auth.refresh(None).await;
    // A permanent rejection resolves as signed-out (not an error): the
    // session can never recover, so the caller stops retrying.
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
    assert!(matches!(auth.state(), AuthState::SignedOut));
    assert!(
        !dir.path().join("session.json").exists(),
        "stored session removed on permanent rejection"
    );
}

#[tokio::test]
async fn refresh_429_preserves_session() {
    let edge = mock_edge_status(
        "429 Too Many Requests",
        r#"{"error":"rate limited","code":"rate_limited","retryable":true}"#,
    )
    .await;
    let (auth, dir) = workos_auth_with_session(&edge);

    let result = auth.refresh(None).await;
    assert!(
        result.is_err(),
        "transient failure surfaces as a retryable error"
    );
    assert!(auth.state().is_signed_in(), "session stays signed in");
    assert_eq!(stored_session(&dir).refresh_token, "rt-1");
}

#[tokio::test]
async fn refresh_5xx_preserves_session() {
    for (status, body) in [
        (
            "500 Internal Server Error",
            r#"{"code":"upstream","retryable":true}"#,
        ),
        (
            "503 Service Unavailable",
            r#"{"code":"upstream","retryable":true}"#,
        ),
    ] {
        let edge = mock_edge_status(status, body).await;
        let (auth, dir) = workos_auth_with_session(&edge);
        let result = auth.refresh(None).await;
        assert!(result.is_err(), "{status} surfaces as an error");
        assert!(auth.state().is_signed_in(), "{status} keeps the session");
        assert_eq!(stored_session(&dir).refresh_token, "rt-1");
    }
}

#[tokio::test]
async fn refresh_malformed_gateway_body_preserves_session() {
    // A 200 with a non-JSON body: the gateway is broken, not the session.
    let edge = mock_edge_status("200 OK", "<html>oops</html>").await;
    let (auth, dir) = workos_auth_with_session(&edge);

    let result = auth.refresh(None).await;
    assert!(result.is_err(), "malformed response surfaces as an error");
    assert!(
        auth.state().is_signed_in(),
        "malformed response keeps the session"
    );
    assert_eq!(stored_session(&dir).refresh_token, "rt-1");
}

#[tokio::test]
async fn refresh_dropped_connection_preserves_session() {
    let edge = mock_edge_dropped().await;
    let (auth, dir) = workos_auth_with_session(&edge);

    let result = auth.refresh(None).await;
    assert!(result.is_err(), "transport failure surfaces as an error");
    assert!(
        auth.state().is_signed_in(),
        "transport failure keeps the session"
    );
    assert_eq!(stored_session(&dir).refresh_token, "rt-1");
}

#[tokio::test]
async fn refresh_401_without_machine_code_preserves_session() {
    // A bare 401 with no recognized `code` is ambiguous, not a confirmed
    // credential rejection — keep the session (conservative direction).
    let edge = mock_edge_status("401 Unauthorized", r#"{"error":"nope"}"#).await;
    let (auth, dir) = workos_auth_with_session(&edge);

    let result = auth.refresh(None).await;
    assert!(result.is_err());
    assert!(
        auth.state().is_signed_in(),
        "ambiguous 401 keeps the session"
    );
    assert_eq!(stored_session(&dir).refresh_token, "rt-1");
}

#[tokio::test]
async fn exchange_failure_surfaces_without_persisting_session() {
    let edge = mock_edge_status(
        "401 Unauthorized",
        r#"{"error":"invalid_grant","code":"invalid_grant","retryable":false}"#,
    )
    .await;
    let (auth, dir) = workos_auth(&edge);
    let url = auth.start_headless_sign_in();
    let state = query_params(&url).remove("state").expect("state present");

    let err = auth
        .complete_sign_in(&format!("{state}.expired-code"))
        .await;
    assert!(err.is_err(), "exchange rejection surfaces as an error");
    assert!(matches!(auth.state(), AuthState::SignedOut));
    assert!(
        !dir.path().join("session.json").exists(),
        "a failed exchange never persists a session"
    );
}

// -- Avatar refresh semantics ---------------------------------------
//
// The refresh response carries optional nested user metadata. `user`
// ABSENT (old edge / omitted) → preserve the stored avatar; `user`
// PRESENT → replace or clear from its sanitized `profilePictureUrl`.
// Changes emit on the state channel so already-signed-in surfaces update
// without a re-login.

/// `workos_auth_with_session` with a persisted avatar on the stored user
/// (so state and disk agree at construction, like a real loaded session).
fn workos_auth_with_avatar(edge_url: &str, avatar_url: Option<&str>) -> (Auth, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let session = StoredSession {
        refresh_token: "rt-1".into(),
        user: AuthUser {
            id: "u1".into(),
            email: "a@b.c".into(),
            name: None,
            avatar_url: avatar_url.map(str::to_string),
        },
        org_id: Some("org_1".into()),
    };
    std::fs::write(
        dir.path().join("session.json"),
        serde_json::to_vec(&session).unwrap(),
    )
    .unwrap();
    let auth = Auth::new(config_with_edge(edge_url, dir.path()));
    assert!(auth.state().is_signed_in());
    (auth, dir)
}

#[tokio::test]
async fn refresh_with_user_metadata_replaces_the_avatar_and_emits() {
    let edge = mock_edge_status(
        "200 OK",
        r#"{"user":{"id":"u1","email":"a@b.c","firstName":"Ann","lastName":"X","profilePictureUrl":"https://avatars.example.com/new.png"},"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMSJ9.sig","refreshToken":"rt2"}"#,
    )
    .await;
    let (auth, dir) = workos_auth_with_avatar(&edge, Some("https://avatars.example.com/old.png"));
    let mut state_rx = auth.watch_state();

    let result = auth.refresh(None).await;
    assert!(result.is_ok());
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        Some("https://avatars.example.com/new.png")
    );
    // Persisted too — the avatar survives a restart.
    assert_eq!(
        stored_session(&dir).user.avatar_url.as_deref(),
        Some("https://avatars.example.com/new.png")
    );
    // The state channel emitted the updated profile (no re-login).
    assert!(state_rx.changed().await.is_ok());
    assert!(matches!(auth.state(), AuthState::SignedIn { .. }));
}

#[tokio::test]
async fn refresh_without_user_metadata_preserves_the_stored_avatar() {
    let edge = mock_edge_status(
        "200 OK",
        r#"{"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMSJ9.sig","refreshToken":"rt2"}"#,
    )
    .await;
    let (auth, dir) = workos_auth_with_avatar(&edge, Some("https://avatars.example.com/keep.png"));
    let mut state_rx = auth.watch_state();

    let result = auth.refresh(None).await;
    assert!(result.is_ok());
    // Old edge / omitted user: the stored avatar survives on state AND disk.
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        Some("https://avatars.example.com/keep.png")
    );
    assert_eq!(
        stored_session(&dir).user.avatar_url.as_deref(),
        Some("https://avatars.example.com/keep.png")
    );
    // Nothing changed → the state channel does NOT re-emit.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), state_rx.changed())
            .await
            .is_err(),
        "no avatar/org change must not emit"
    );
}

#[tokio::test]
async fn refresh_with_explicit_null_clears_the_avatar() {
    let edge = mock_edge_status(
        "200 OK",
        r#"{"user":{"id":"u1","email":"a@b.c","firstName":"Ann","lastName":"X","profilePictureUrl":null},"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMSJ9.sig","refreshToken":"rt2"}"#,
    )
    .await;
    let (auth, dir) = workos_auth_with_avatar(&edge, Some("https://avatars.example.com/gone.png"));

    let result = auth.refresh(None).await;
    assert!(result.is_ok());
    // The user is present with no picture: an explicit clear.
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        None
    );
    assert_eq!(stored_session(&dir).user.avatar_url, None);
}

// -- Loaded-session avatar boundary --------------------------------
//
// session.json serializes the user camelCase (`avatarUrl`). A valid URL
// is parsed and lands in state AND inner.stored (persisted too); an
// unsafe URL is parsed, sanitized to None everywhere, and rewritten out
// of the file — so a later org-changing refresh can never re-emit it.

#[test]
fn loaded_session_parses_and_persists_a_valid_camelcase_avatar() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.json"),
        r#"{"refreshToken":"rt-1","user":{"id":"u1","email":"a@b.c","avatarUrl":"https://avatars.example.com/a.png"},"orgId":"org_1"}"#,
    )
    .unwrap();
    let auth = Auth::new(config_with_edge("http://edge.test", dir.path()));
    // Initial state carries the avatar.
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        Some("https://avatars.example.com/a.png")
    );
    // `inner.stored` holds the SAME clean value (nothing re-emittable).
    let stored = stored_session(&dir);
    assert_eq!(
        stored.user.avatar_url.as_deref(),
        Some("https://avatars.example.com/a.png")
    );
    // session.json serializes camelCase — never snake_case.
    let raw = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
    assert!(
        raw.contains("avatarUrl"),
        "session.json must use avatarUrl: {raw}"
    );
    assert!(
        !raw.contains("avatar_url"),
        "session.json must not use avatar_url: {raw}"
    );
}

#[test]
fn loaded_session_parses_then_sanitizes_an_unsafe_avatar_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.json"),
        r#"{"refreshToken":"rt-1","user":{"id":"u1","email":"a@b.c","avatarUrl":"http://evil.example/a.png"},"orgId":"org_1"}"#,
    )
    .unwrap();
    let auth = Auth::new(config_with_edge("http://edge.test", dir.path()));
    // The unsafe URL IS parsed (it was really read in), then sanitized to
    // None everywhere: initial state, inner.stored, and the persisted file.
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        None
    );
    assert_eq!(stored_session(&dir).user.avatar_url, None);
    let raw = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
    assert!(
        !raw.contains("evil.example"),
        "unsafe URL must be rewritten out: {raw}"
    );
    assert!(
        !raw.contains("avatarUrl"),
        "cleaned session omits the avatar: {raw}"
    );
}

/// Gap-2 regression: an unsafe avatar must never survive in `inner.stored`
/// and get re-emitted by a later org-changing refresh.
#[tokio::test]
async fn unsafe_loaded_avatar_never_reemits_after_org_changing_refresh() {
    let edge = mock_edge_status(
        "200 OK",
        r#"{"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMiJ9.sig","refreshToken":"rt2"}"#,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.json"),
        r#"{"refreshToken":"rt-1","user":{"id":"u1","email":"a@b.c","avatarUrl":"http://evil.example/a.png"},"orgId":"org_1"}"#,
    )
    .unwrap();
    let auth = Auth::new(config_with_edge(&edge, dir.path()));
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        None,
        "unsafe avatar sanitized at load"
    );
    // A refresh that changes org (org_1 → org_2) emits a new SignedIn
    // state built from `inner.stored` — which must be the sanitized user.
    let result = auth.refresh(None).await;
    assert!(result.is_ok());
    assert_eq!(auth.state().org_id(), Some("org_2"));
    assert_eq!(
        auth.state().user().and_then(|u| u.avatar_url.as_deref()),
        None,
        "unsafe avatar must not re-emit through an org-changing refresh"
    );
}
