//! The outgoing scrub (docs/design/agent-bots.md §11.1): before an answer
//! leaves the host, redact strings that look like secrets. It is a safety
//! net, not a guarantee: it knows private-key blocks, common token formats
//! and `.env`-style assignments, and nothing else.

use std::sync::LazyLock;

use regex::Regex;

const REDACTED: &str = "[redacted]";

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    // A block with no END line is redacted to the end of the text.
    Regex::new(
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
    )
    .expect("private key pattern")
});

/// Token formats with a recognisable shape.
static TOKENS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // AWS access key ids.
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        // GitHub tokens.
        r"\bgh[pousr]_[A-Za-z0-9]{30,}\b",
        r"\bgithub_pat_[A-Za-z0-9_]{22,}\b",
        // Slack tokens.
        r"\bxox[abposr]-[A-Za-z0-9-]{10,}\b",
        // OpenAI, Anthropic and similar `sk-` keys.
        r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}",
        // Stripe keys.
        r"\b[rsp]k_(?:live|test)_[0-9A-Za-z]{16,}\b",
        // Google API keys.
        r"\bAIza[0-9A-Za-z_-]{35}\b",
        // JSON Web Tokens.
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("token pattern"))
    .collect()
});

static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(bearer|token)(\s+)[A-Za-z0-9._~+/=-]{20,}").expect("bearer pattern")
});

/// `scheme://user:password@host`.
static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/:@]+:[^\s/@]+@").expect("url pattern")
});

/// Names that mark a value as secret.
const SECRET_NAME: &str =
    r"(?i:secret|token|passw(?:or)?d|pwd|api_?key|private_?key|credential|access_?key|auth_?key)";

/// `.env` lines: `[export] NAME=value` where NAME looks secret.
static ENV_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?m)^(\s*(?:export\s+)?(?:[A-Za-z_][A-Za-z0-9_]*)?{SECRET_NAME}[A-Za-z0-9_]*\s*=\s*)("[^"\n]*"|'[^'\n]*'|[^\s#]+)"#
    ))
    .expect("env pattern")
});

/// JSON and YAML pairs with a quoted value: `"apiKey": "…"`.
static QUOTED_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(["']?[A-Za-z0-9_-]*{SECRET_NAME}[A-Za-z0-9_-]*["']?\s*:\s*)("[^"\n]{{4,}}"|'[^'\n]{{4,}}')"#
    ))
    .expect("pair pattern")
});

/// `text` with secret-looking strings redacted, and how many were.
pub fn scrub(text: &str) -> (String, usize) {
    let mut count = 0;
    let mut out = replace(&PRIVATE_KEY, text, &mut count, |_| {
        "[redacted private key]".to_string()
    });
    for pattern in TOKENS.iter() {
        out = replace(pattern, &out, &mut count, |_| REDACTED.to_string());
    }
    out = replace(&BEARER, &out, &mut count, |caps| {
        format!("{}{}{REDACTED}", &caps[1], &caps[2])
    });
    out = replace(&URL_CREDENTIALS, &out, &mut count, |caps| {
        format!("{}{REDACTED}@", &caps[1])
    });
    out = replace(&ENV_ASSIGNMENT, &out, &mut count, |caps| {
        format!("{}{REDACTED}", &caps[1])
    });
    out = replace(&QUOTED_PAIR, &out, &mut count, |caps| {
        format!("{}\"{REDACTED}\"", &caps[1])
    });
    (out, count)
}

fn replace(
    pattern: &Regex,
    text: &str,
    count: &mut usize,
    with: impl Fn(&regex::Captures) -> String,
) -> String {
    pattern
        .replace_all(text, |caps: &regex::Captures| {
            // A value an earlier pattern already redacted is not counted twice.
            if caps[0].ends_with(REDACTED) || caps[0].ends_with(&format!("\"{REDACTED}\"")) {
                return caps[0].to_string();
            }
            *count += 1;
            with(caps)
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_key_blocks_go_whole() {
        let text = "Key:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nThat's it.";
        let (out, n) = scrub(text);
        assert_eq!(out, "Key:\n[redacted private key]\nThat's it.");
        assert_eq!(n, 1);
        let (cut, _) = scrub("-----BEGIN RSA PRIVATE KEY-----\nMIIE…");
        assert_eq!(cut, "[redacted private key]");
        let (public, n) = scrub("-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----");
        assert_eq!(n, 0, "public keys stay: {public}");
    }

    #[test]
    fn common_token_formats() {
        for token in [
            "AKIAIOSFODNN7EXAMPLE",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "xoxb-1234567890-abcdefghij",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345",
            "sk-proj-abcdefghijklmnopqrstuvwxyz",
            "sk_live_abcdefghijklmnop1234",
            "AIzaSyA1234567890abcdefghijklmnopqrstuv",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        ] {
            let (out, n) = scrub(&format!("use {token} for it"));
            assert_eq!(out, "use [redacted] for it", "{token}");
            assert_eq!(n, 1, "{token}");
        }
        let (out, _) = scrub("curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz.123'");
        assert_eq!(out, "curl -H 'Authorization: Bearer [redacted]'");
        let (out, _) = scrub("postgres://admin:hunter22@db.internal:5432/app");
        assert_eq!(out, "postgres://[redacted]@db.internal:5432/app");
    }

    #[test]
    fn env_style_assignments_keep_their_names() {
        let text = "Set these:\nDATABASE_PASSWORD=hunter2\nexport STRIPE_SECRET_KEY=\"abc def\"\nAPI_KEY = 'xyz' # prod\nPORT=8080\nLOG_LEVEL=debug";
        let (out, n) = scrub(text);
        assert_eq!(
            out,
            "Set these:\nDATABASE_PASSWORD=[redacted]\nexport STRIPE_SECRET_KEY=[redacted]\nAPI_KEY = [redacted] # prod\nPORT=8080\nLOG_LEVEL=debug"
        );
        assert_eq!(n, 3);
        let (out, n) = scrub(r#"{"apiKey": "abcd1234", "name": "web", "password": 'p4ss-word'}"#);
        assert_eq!(
            out,
            r#"{"apiKey": "[redacted]", "name": "web", "password": "[redacted]"}"#
        );
        assert_eq!(n, 2);
    }

    #[test]
    fn ordinary_prose_is_left_alone() {
        let text = "The auth token refresh landed on Tuesday; passwords are hashed with argon2. \
                    See cypher://session/abc-123 and commit 3f2a9c1. Token: none needed.";
        assert_eq!(scrub(text), (text.to_string(), 0));
    }
}
