//! The sign-in loopback: a hand-rolled HTTP listener (no HTTP server
//! dependency in the engine) for the OAuth callback and the email
//! verification form, plus the pages it serves.

use std::collections::HashMap;
use std::sync::Weak;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{Auth, AuthInner, ExchangeOutcome, url_decode};

pub(super) async fn loopback_loop(listener: tokio::net::TcpListener, inner: Weak<AuthInner>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let Some(inner) = inner.upgrade() else { break };
        tokio::spawn(async move {
            if let Err(err) = handle_loopback_conn(stream, Auth { inner }).await {
                tracing::debug!(error = %err, "auth: callback connection failed");
            }
        });
    }
}

/// One loopback request: its method and path, and the query and URL-encoded
/// form parameters.
struct LoopbackRequest {
    method: String,
    path: String,
    query: HashMap<String, String>,
    form: HashMap<String, String>,
}

async fn handle_loopback_conn(
    mut stream: tokio::net::TcpStream,
    auth: Auth,
) -> Result<(), std::io::Error> {
    let request = read_loopback_request(&mut stream).await?;
    let (status, body) = match request.path.as_str() {
        "/callback" => callback_response(&auth, &request.query).await,
        "/verify" => {
            let params = if request.method == "POST" {
                &request.form
            } else {
                &request.query
            };
            verify_response(&auth, params).await
        }
        _ => ("404 Not Found", page("Not found.")),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

async fn read_loopback_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<LoopbackRequest, std::io::Error> {
    // Read the request head (bounded; verification submissions also carry a
    // small URL-encoded body).
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "header read"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|offset| offset + 4)
        .unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let (method, target) = {
        let request_line = head.lines().next().unwrap_or_default();
        (
            request_line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_owned(),
            request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_owned(),
        )
    };
    let content_length = head
        .lines()
        .find_map(|line| {
            line.strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
        })
        .and_then(|length| length.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > 16 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request body too large",
        ));
    }
    let body_start = header_end;
    while buf.len() < body_start + content_length {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "body read"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let form_body = String::from_utf8_lossy(
        &buf[body_start..buf.len().min(body_start.saturating_add(content_length))],
    )
    .into_owned();
    let (path, query) = target
        .as_str()
        .split_once('?')
        .unwrap_or((target.as_str(), ""));

    let query: HashMap<String, String> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), url_decode(v)))
        .collect();
    let form: HashMap<String, String> = form_body
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), url_decode(v)))
        .collect();
    Ok(LoopbackRequest {
        method,
        path: path.to_string(),
        query,
        form,
    })
}

fn invalid_callback() -> (&'static str, String) {
    (
        "400 Bad Request",
        page("Invalid or expired sign-in link. Start again from Cypher."),
    )
}

/// `/callback`: the OAuth redirect — exchange the code for the pending sign-in.
async fn callback_response(auth: &Auth, query: &HashMap<String, String>) -> (&'static str, String) {
    let code = query.get("code");
    let state = query.get("state");
    match (code, state) {
        (Some(code), Some(state)) => match auth.take_pending(state) {
            Some((generation, verifier)) => match auth.exchange_code(code, &verifier).await {
                Ok(ExchangeOutcome::Complete(result)) => {
                    match auth.finish_sign_in(result, generation) {
                        Ok(()) => (
                            "200 OK",
                            page("Signed in. You can close this tab and return to Cypher."),
                        ),
                        Err(err) => {
                            tracing::info!(
                                error = %err,
                                "auth: discarded canceled callback exchange"
                            );
                            (
                                "409 Conflict",
                                page(
                                    "This sign-in was canceled. Start again from Cypher if you still want to enable sync.",
                                ),
                            )
                        }
                    }
                }
                Ok(ExchangeOutcome::EmailVerificationRequired {
                    pending_authentication_token,
                    email,
                }) => {
                    auth.store_email_verification(
                        state,
                        generation,
                        pending_authentication_token,
                        email.clone(),
                    );
                    ("200 OK", verification_page(state, email.as_deref(), None))
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "auth: loopback code exchange failed"
                    );
                    (
                        "502 Bad Gateway",
                        page("Sign-in failed during token exchange — check the Cypher logs."),
                    )
                }
            },
            None => invalid_callback(),
        },
        _ => invalid_callback(),
    }
}

/// `/verify`: the emailed code for a sign-in WorkOS challenged.
async fn verify_response(auth: &Auth, params: &HashMap<String, String>) -> (&'static str, String) {
    let state = params.get("state");
    let code = params.get("code");
    match (state, code) {
        (Some(state), Some(code))
            if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) =>
        {
            match auth.pending_email_verification(state) {
                Some((generation, token, email)) => {
                    match auth.exchange_email_verification(&token, code).await {
                        Ok(result) => match auth.finish_sign_in(result, generation) {
                            Ok(()) => {
                                auth.clear_email_verification(state);
                                (
                                    "200 OK",
                                    page(
                                        "Email verified and signed in. You can close this tab and return to Cypher.",
                                    ),
                                )
                            }
                            Err(err) => {
                                auth.clear_email_verification(state);
                                tracing::info!(
                                    error = %err,
                                    "auth: discarded canceled email verification"
                                );
                                (
                                    "409 Conflict",
                                    page(
                                        "This sign-in was canceled. Start again from Cypher if you still want to enable sync.",
                                    ),
                                )
                            }
                        },
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "auth: email verification failed"
                            );
                            (
                                "400 Bad Request",
                                verification_page(
                                    state,
                                    email.as_deref(),
                                    Some(
                                        "That code was not accepted. Check the email and try again.",
                                    ),
                                ),
                            )
                        }
                    }
                }
                None => invalid_callback(),
            }
        }
        (Some(state), _) if auth.pending_email_verification(state).is_some() => {
            let email = auth
                .pending_email_verification(state)
                .and_then(|(_, _, email)| email);
            (
                "400 Bad Request",
                verification_page(
                    state,
                    email.as_deref(),
                    Some("Enter the six-digit code from your email."),
                ),
            )
        }
        _ => invalid_callback(),
    }
}

/// Shared shell for every loopback page — mirrors the edge's hosted
/// paste-code page (dark, centered card, system fonts) so the browser side of
/// sign-in looks like one product regardless of which server rendered it.
/// Inline styles only: these pages must never fetch external assets.
fn page_shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\
<html lang='en'>\
<head>\
<meta charset='utf-8'>\
<meta name='viewport' content='width=device-width, initial-scale=1'>\
<meta name='referrer' content='no-referrer'>\
<meta name='robots' content='noindex'>\
<title>{title}</title>\
<style>\
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center;\
         background: #0a0a0a; color: #ededed;\
         font: 15px/1.6 ui-sans-serif, system-ui, sans-serif; }}\
  main {{ max-width: 24rem; padding: 2.5rem 2rem; text-align: center; }}\
  .mark {{ width: 44px; height: 44px; margin: 0 auto 1.25rem; border-radius: 12px;\
         background: #ededed; color: #0a0a0a; display: grid; place-items: center;\
         font: 700 20px ui-sans-serif, system-ui, sans-serif; }}\
  h1 {{ font-size: 1.05rem; font-weight: 600; margin: 0 0 0.75rem; }}\
  p {{ color: #a1a1a1; margin: 0.25rem 0; }}\
  p.error {{ color: #f87171; }}\
  strong {{ color: #ededed; font-weight: 600; }}\
  form {{ margin-top: 1.25rem; }}\
  input[name=code] {{ display: block; width: 100%; box-sizing: border-box;\
         margin: 0 0 0.75rem; padding: 0.8rem 1rem; text-align: center;\
         background: #171717; border: 1px solid #2e2e2e; border-radius: 10px;\
         color: #ededed; font: 600 22px/1.4 ui-monospace, monospace;\
         letter-spacing: 0.45em; text-indent: 0.45em; outline: none; }}\
  input[name=code]:focus {{ border-color: #ededed; }}\
  button {{ width: 100%; padding: 0.7rem 1rem; border-radius: 10px; border: none;\
         background: #ededed; color: #0a0a0a; cursor: pointer;\
         font: 600 14px ui-sans-serif, system-ui, sans-serif; }}\
  button:hover {{ background: #ffffff; }}\
  .hint {{ margin-top: 1.25rem; font-size: 13px; }}\
</style>\
</head>\
<body><main><div class='mark' aria-hidden='true'>C</div>{body}</main></body>\
</html>"
    )
}

fn page(message: &str) -> String {
    // Split "Title. Rest of the message." into heading + body when possible.
    let (title, rest) = message.split_once(". ").unwrap_or((message, ""));
    let title = title.trim_end_matches('.');
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!("<p>{}</p>", escape_html(rest))
    };
    page_shell(
        &escape_html(title),
        &format!("<h1>{}</h1>{rest}", escape_html(title)),
    )
}

fn verification_page(state: &str, email: Option<&str>, error: Option<&str>) -> String {
    let email = email
        .map(escape_html)
        .unwrap_or_else(|| "your email".into());
    let error = error
        .map(|message| format!("<p class='error'>{}</p>", escape_html(message)))
        .unwrap_or_default();
    page_shell(
        "Verify your email",
        &format!(
            "<h1>Verify your email</h1>\
<p>We sent a six-digit code to <strong>{email}</strong>.</p>\
{error}\
<form action='/verify' method='post'>\
<input type='hidden' name='state' value='{}'>\
<input name='code' inputmode='numeric' autocomplete='one-time-code' \
pattern='[0-9]{{6}}' maxlength='6' placeholder='000000' required autofocus \
aria-label='Verification code'>\
<button type='submit'>Verify and sign in</button>\
</form>\
<p class='hint'>The code expires in a few minutes. Keep this tab open until you're signed in.</p>",
            escape_html(state)
        ),
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
