//! cypher-env — environment resolution for the Cypher product.
//!
//! Every environment variable is read under the `CYPHER_*` family. The legacy
//! `ZERON_*` family is no longer read — the product fully cut over to Cypher.

use std::ffi::OsString;
use std::path::PathBuf;

/// Read `CYPHER_{suffix}`. Empty/whitespace values read as unset (so an
/// explicit `CYPHER_X=""` can force a dev-mode default the same way an absent
/// value would).
pub fn var(suffix: &str) -> Option<String> {
    std::env::var(format!("CYPHER_{suffix}"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// OsString variant of [`var`] — empty values read as unset.
pub fn var_os(suffix: &str) -> Option<OsString> {
    std::env::var_os(format!("CYPHER_{suffix}")).filter(|s| !s.is_empty())
}

/// The default data root: explicit `CYPHER_DATA_DIR`, else `~/.cypher`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = var_os("DATA_DIR") {
        return PathBuf::from(dir);
    }
    home_dir().join(".cypher")
}

/// Best-effort home directory, shared with the engine's path helpers.
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Worktree root: explicit `CYPHER_WORKTREES_DIR`, else `~/.cypher/worktrees`.
pub fn worktrees_dir() -> PathBuf {
    if let Some(dir) = var_os("WORKTREES_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    home_dir().join(".cypher").join("worktrees")
}

/// Adapters prefix: explicit `CYPHER_ADAPTERS_DIR`, else `~/.cypher/adapters`.
pub fn adapters_dir() -> Option<PathBuf> {
    if let Some(dir) = var_os("ADAPTERS_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    Some(home_dir().join(".cypher").join("adapters"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(k: &str, v: Option<&str>) {
        match v {
            Some(v) => unsafe { std::env::set_var(k, v) },
            None => unsafe { std::env::remove_var(k) },
        }
    }

    #[test]
    fn var_reads_only_cypher_family() {
        set("CYPHER_TEST_VAR", Some("new"));
        set("ZERON_TEST_VAR", Some("old"));
        assert_eq!(var("TEST_VAR").as_deref(), Some("new"));
        set("CYPHER_TEST_VAR", None);
        // The legacy ZERON_* family is no longer read.
        assert_eq!(var("TEST_VAR"), None);
        set("ZERON_TEST_VAR", None);
    }

    #[test]
    fn empty_cypher_reads_as_unset() {
        set("CYPHER_TEST_VAR", Some("  "));
        assert_eq!(var("TEST_VAR"), None);
        set("CYPHER_TEST_VAR", None);
    }

    #[test]
    fn data_dir_defaults_to_cypher_only() {
        set("CYPHER_DATA_DIR", Some("/tmp/cypher-data"));
        assert_eq!(data_dir(), PathBuf::from("/tmp/cypher-data"));

        set("CYPHER_DATA_DIR", None);
        set("ZERON_DATA_DIR", Some("/tmp/zeron-data"));
        // Legacy ZERON_DATA_DIR is ignored; the ~/.cypher default applies.
        assert_eq!(data_dir(), home_dir().join(".cypher"));
        set("ZERON_DATA_DIR", None);
    }

    #[test]
    fn worktrees_and_adapters_are_cypher_only() {
        set("CYPHER_WORKTREES_DIR", Some("/tmp/wt"));
        assert_eq!(worktrees_dir(), PathBuf::from("/tmp/wt"));
        set("CYPHER_WORKTREES_DIR", None);
        assert_eq!(
            worktrees_dir(),
            home_dir().join(".cypher").join("worktrees")
        );

        set("CYPHER_ADAPTERS_DIR", Some("/tmp/ad"));
        assert_eq!(adapters_dir(), Some(PathBuf::from("/tmp/ad")));
        set("CYPHER_ADAPTERS_DIR", None);
        assert_eq!(
            adapters_dir(),
            Some(home_dir().join(".cypher").join("adapters"))
        );
    }
}
