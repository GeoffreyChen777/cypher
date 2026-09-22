//! Exercise the actual process configuration, not just Cargo feature strings.
use std::process::{Command, Output};

fn run(extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cypher"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CYPHER_") {
            cmd.env_remove(key);
        }
    }
    cmd.args(["status", "--verbose"]);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

#[test]
fn invalid_profiles_and_misplaced_secrets_fail_closed() {
    assert!(!run(&[("CYPHER_PROFILE", "typo")]).status.success());
    let secret = "a".repeat(64);
    let result = run(&[("CYPHER_DEV_ACCESS_TOKEN", &secret)]);
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&secret));
}

#[test]
fn development_requires_the_feature_and_its_own_data_root() {
    let result = run(&[("CYPHER_PROFILE", "development")]);
    assert!(!result.status.success());
    if !cfg!(feature = "development") {
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("does not support development Edge authentication")
        );
    }
    let secret = "b".repeat(64);
    let result = run(&[
        ("CYPHER_PROFILE", "development"),
        ("CYPHER_DEV_EDGE_URL", "https://edge-dev.letscypher.app"),
        ("CYPHER_DEV_ACCESS_TOKEN", &secret),
        ("CYPHER_DATA_DIR", "/tmp/unsafe-profile"),
    ]);
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&secret));
}

#[test]
fn development_edge_cannot_be_selected_as_a_production_url_override() {
    // Reaching the development Edge requires CYPHER_PROFILE=development; it may
    // never be selected through the ordinary production URL override, which
    // would pair a development endpoint with production auth resolution.
    for suffix in ["", "/"] {
        let url = format!("http://127.0.0.1:27640{suffix}");
        assert!(!run(&[("CYPHER_EDGE_URL", &url)]).status.success(), "{url}");
    }
}

#[test]
fn development_edge_url_override_is_restricted_to_development_endpoints() {
    let secret = "c".repeat(64);
    let dir = tempfile::tempdir().unwrap();
    let base: Vec<(&str, &str)> = vec![
        ("CYPHER_PROFILE", "development"),
        ("CYPHER_DEV_ACCESS_TOKEN", secret.as_str()),
        ("CYPHER_DATA_DIR", dir.path().to_str().unwrap()),
    ];

    // The production Edge, plaintext off loopback, and junk are all rejected
    // on the URL itself — a development bearer must not travel to them.
    for bad in [
        "https://edge.letscypher.app",
        "http://edge-dev.letscypher.app",
        "not-a-url",
    ] {
        let mut env = base.clone();
        env.push(("CYPHER_DEV_EDGE_URL", bad));
        let result = run(&env);
        assert!(!result.status.success(), "{bad} must not be accepted");
        if cfg!(feature = "development") {
            assert!(
                String::from_utf8_lossy(&result.stderr).contains("CYPHER_DEV_EDGE_URL"),
                "{bad} should fail on the URL rule"
            );
        }
        assert!(!String::from_utf8_lossy(&result.stderr).contains(&secret));
    }

    // A well-formed custom endpoint clears the URL rule and is then held to
    // the same private-data-root requirement as the built-in default.
    let mut env = base.clone();
    env.push(("CYPHER_DEV_EDGE_URL", "https://edge-dev.letscypher.app"));
    let result = run(&env);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    if cfg!(feature = "development") {
        assert!(
            !stderr.contains("CYPHER_DEV_EDGE_URL"),
            "URL rule should have passed"
        );
        assert!(
            stderr.contains(".cypher-development"),
            "expected the data-root rule"
        );
    }
    assert!(!stderr.contains(&secret));
}

/// A loopback `wrangler dev` runs `AUTH_MODE=dev`, where the bearer is the
/// identity and only `user@org` carries the org claim `/registry/:orgId/*`
/// compares against the URL. The 64-hex secret shape cannot express one, so it
/// is accepted but not required there — and still required everywhere else.
#[test]
fn a_dev_identity_bearer_is_accepted_on_loopback_only() {
    if !cfg!(feature = "development") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_str().unwrap().to_string();
    // Every case below stops at the private-data-root rule, which runs after
    // the credential rule: reaching that later rule is how a credential proves
    // it was accepted.
    let rejected_credential = |result: &Output| {
        String::from_utf8_lossy(&result.stderr)
            .contains("Missing or invalid development credential")
    };
    let run_with = |edge: Option<&str>, token: &str| {
        let mut env = vec![
            ("CYPHER_PROFILE", "development"),
            ("CYPHER_DEV_ACCESS_TOKEN", token),
            ("CYPHER_DATA_DIR", data.as_str()),
        ];
        if let Some(edge) = edge {
            env.push(("CYPHER_DEV_EDGE_URL", edge));
        }
        run(&env)
    };

    // The built-in default is loopback, as is an explicit loopback override.
    for edge in [
        None,
        Some("http://127.0.0.1:27640"),
        Some("http://localhost:27640"),
    ] {
        let result = run_with(edge, "dev-user@dev-org");
        assert!(!result.status.success());
        assert!(
            !rejected_credential(&result),
            "{edge:?} should accept a user@org identity"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(".cypher-development"),
            "{edge:?} should stop at the data-root rule instead"
        );
    }

    // A remote endpoint still takes that deployment's shared secret, only.
    let result = run_with(Some("https://edge-dev.letscypher.app"), "dev-user@dev-org");
    assert!(!result.status.success());
    assert!(
        rejected_credential(&result),
        "a remote Edge must still require the 64-hex secret"
    );

    // Shapes a local Edge could not split into one unambiguous identity.
    for bad in [
        "dev-user",
        "@dev-org",
        "dev-user@",
        "a@b@c",
        "dev user@dev-org",
        "",
    ] {
        let result = run_with(Some("http://127.0.0.1:27640"), bad);
        assert!(!result.status.success(), "{bad:?}");
        assert!(rejected_credential(&result), "{bad:?} must be rejected");
    }

    // The secret remains valid on loopback, and is still never echoed.
    let secret = "d".repeat(64);
    let result = run_with(Some("http://127.0.0.1:27640"), &secret);
    assert!(!result.status.success());
    assert!(!rejected_credential(&result));
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&secret));
}

#[test]
fn local_profile_does_not_silently_restore_a_cloud_login() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("session.json"), "{}").unwrap();
    let result = run(&[
        ("CYPHER_PROFILE", "local"),
        ("CYPHER_DATA_DIR", dir.path().to_str().unwrap()),
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Local profile cannot load"));
}
