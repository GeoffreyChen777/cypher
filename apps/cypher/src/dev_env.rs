//! The development-profile guard: which Edge a development build may target
//! and which credentials and data directory it may use there.

use crate::PRODUCTION_EDGE_URL;

/// The development Edge this build defaults to: a local `wrangler dev`, on the
/// port `edge/package.json`'s `dev` script binds (there is no hosted
/// development Worker).
///
/// `CYPHER_DEV_EDGE_URL` overrides this, so a development engine can still be
/// pointed at a self-hosted staging server without a rebuild.
pub const DEFAULT_DEVELOPMENT_EDGE_URL: &str = "http://127.0.0.1:27640";

/// The development Edge this process targets.
pub fn development_edge_url() -> String {
    cypher_env::var("DEV_EDGE_URL").unwrap_or_else(|| DEFAULT_DEVELOPMENT_EDGE_URL.into())
}

/// The `(plaintext, host)` pair of an Edge URL, or `None` if it is not a
/// well-formed http(s) URL.
fn edge_scheme_and_host(url: &str) -> Option<(bool, &str)> {
    let url = url.trim_end_matches('/');
    let (plaintext, rest) = match url.split_once("://") {
        Some(("https", rest)) => (false, rest),
        Some(("http", rest)) => (true, rest),
        _ => return None,
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        // Strip a trailing numeric port only. A bracketed IPv6 literal with no
        // port (`[::1]`) splits into a non-numeric tail and is left intact.
        Some((head, port))
            if !head.is_empty() && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            head
        }
        _ => authority,
    };
    Some((plaintext, host))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Whether the development Edge is this machine — the one place a development
/// bearer never leaves the host.
fn development_edge_is_loopback(url: &str) -> bool {
    edge_scheme_and_host(url).is_some_and(|(_, host)| is_loopback_host(host))
}

/// A development bearer is a shared secret for one deployment, so it may only
/// travel to a development endpoint: never the production Edge, and never over
/// plaintext to anything but loopback. This is the guard that makes the URL
/// safe to take from the environment at all.
fn development_edge_is_safe(url: &str) -> bool {
    if url
        .trim_end_matches('/')
        .eq_ignore_ascii_case(PRODUCTION_EDGE_URL)
    {
        return false;
    }
    match edge_scheme_and_host(url) {
        Some((true, host)) => is_loopback_host(host),
        Some((false, _)) => true,
        None => false,
    }
}

/// A remote development Edge authenticates with that deployment's shared
/// 64-hex secret, and nothing else is accepted there.
///
/// A loopback `wrangler dev` is a different contract: it runs `AUTH_MODE=dev`,
/// where the bearer *is* the identity and only a `user@org` form carries the
/// org claim that `/registry/:orgId/*` compares against the URL. A 64-hex
/// string cannot express one, so the secret alone authenticates as a user with
/// no org and every registry route answers 403. There is also no shared secret
/// on loopback to protect. Accept either shape there; the secret only, anywhere
/// else.
fn development_credential_is_valid(token: &str, edge: &str) -> bool {
    if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    development_edge_is_loopback(edge) && development_identity_is_valid(token)
}

/// The `user@org` bearer `AUTH_MODE=dev` splits on its first `@`. Exactly one
/// separator is required so the identity a local Edge derives is unambiguous.
fn development_identity_is_valid(token: &str) -> bool {
    let Some((user, org)) = token.split_once('@') else {
        return false;
    };
    !user.is_empty()
        && !org.is_empty()
        && !org.contains('@')
        && token.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

pub fn validate_development_environment() -> anyhow::Result<()> {
    let profile = cypher_env::var("PROFILE").unwrap_or_else(|| "production".into());
    anyhow::ensure!(
        matches!(profile.as_str(), "production" | "local" | "development"),
        "Unknown CYPHER_PROFILE"
    );
    if profile == "local" {
        anyhow::ensure!(
            !cypher_env::data_dir().join("session.json").exists(),
            "Local profile cannot load a saved cloud login; choose an isolated local data directory"
        );
    }
    let development_edge = development_edge_url();
    if profile == "development"
        || cypher_env::var("EDGE_URL").is_some_and(|url| {
            let url = url.trim_end_matches('/');
            url.eq_ignore_ascii_case(development_edge.trim_end_matches('/'))
                || url.eq_ignore_ascii_case(DEFAULT_DEVELOPMENT_EDGE_URL)
        })
    {
        anyhow::ensure!(
            cfg!(feature = "development"),
            "This build does not support development Edge authentication"
        );
        anyhow::ensure!(
            profile == "development",
            "Development Edge requires CYPHER_PROFILE=development"
        );
        anyhow::ensure!(
            development_edge_is_safe(&development_edge),
            "CYPHER_DEV_EDGE_URL must be an https development endpoint (http only on loopback), never the production Edge"
        );
        let token = cypher_env::var("DEV_ACCESS_TOKEN").unwrap_or_default();
        anyhow::ensure!(
            development_credential_is_valid(&token, &development_edge),
            "Missing or invalid development credential"
        );
        let data = cypher_env::canonical_data_dir(&cypher_env::data_dir())?;
        let root =
            cypher_env::canonical_data_dir(&cypher_env::home_dir().join(".cypher-development"))?;
        anyhow::ensure!(
            data.starts_with(&root) && data != root,
            "Development profiles require a private instance under ~/.cypher-development/"
        );
        anyhow::ensure!(
            cypher_env::var("EDGE_TOKEN").is_none(),
            "Do not use the legacy EDGE_TOKEN with locked development auth"
        );
    } else {
        anyhow::ensure!(
            cypher_env::var("DEV_ACCESS_TOKEN").is_none(),
            "Development credentials require the development profile"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_DEVELOPMENT_EDGE_URL, PRODUCTION_EDGE_URL, development_edge_is_safe};

    #[test]
    fn accepts_https_development_endpoints() {
        for url in [
            DEFAULT_DEVELOPMENT_EDGE_URL,
            "https://edge-dev.letscypher.app/",
            "https://edge-dev.letscypher.app",
            "https://edge-staging.example.com:8443",
            "https://192.0.2.10",
        ] {
            assert!(development_edge_is_safe(url), "should accept {url}");
        }
    }

    #[test]
    fn rejects_production_even_with_a_trailing_slash_or_odd_case() {
        for url in [
            PRODUCTION_EDGE_URL,
            "https://edge.letscypher.app/",
            "HTTPS://EDGE.LETSCYPHER.APP",
        ] {
            assert!(!development_edge_is_safe(url), "should reject {url}");
        }
    }

    #[test]
    fn plaintext_is_loopback_only() {
        for url in [
            "http://localhost:27640",
            "http://127.0.0.1:8787",
            "http://[::1]:27640",
        ] {
            assert!(development_edge_is_safe(url), "should accept {url}");
        }
        for url in [
            "http://edge-dev.letscypher.app",
            "http://192.0.2.10",
            "http://evil.example.com",
        ] {
            assert!(
                !development_edge_is_safe(url),
                "should reject plaintext {url}"
            );
        }
    }

    #[test]
    fn rejects_malformed_or_schemeless_values() {
        for url in [
            "",
            "edge-dev.letscypher.app",
            "ftp://edge-dev.letscypher.app",
            "https://",
            "https:///path",
        ] {
            assert!(!development_edge_is_safe(url), "should reject {url}");
        }
    }
}
