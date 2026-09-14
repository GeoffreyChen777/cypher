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
        (
            "CYPHER_DEV_EDGE_URL",
            "https://cypher-edge-development.geoffreychen777.workers.dev",
        ),
        ("CYPHER_DEV_ACCESS_TOKEN", &secret),
        ("CYPHER_DATA_DIR", "/tmp/unsafe-profile"),
    ]);
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&secret));
}

#[test]
fn development_edge_cannot_be_selected_as_a_production_url_override() {
    for suffix in ["", "/"] {
        let url = format!("https://cypher-edge-development.geoffreychen777.workers.dev{suffix}");
        assert!(!run(&[("CYPHER_EDGE_URL", &url)]).status.success());
    }
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
