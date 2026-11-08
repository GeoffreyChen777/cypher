//! cypher-secrets — where Cypher keeps a credential at rest.
//!
//! Every secret has a home file, written 0600. On macOS, when the running
//! binary carries an Apple-anchored signature (release builds: Developer ID
//! with the hardened runtime), the secret lives in the login Keychain instead.
//! There only code with Cypher's signature reads it without a prompt; any
//! other same-user process — an agent's shell running `security`, say — makes
//! macOS ask the user first. Ad-hoc signed development builds keep the file,
//! because each rebuild is a new code identity and a Keychain item would
//! prompt every time. Linux keeps the file too: its secret services hand any
//! same-user process every secret without asking.
//!
//! The file is authoritative whenever it exists. A Keychain write that fails
//! (a locked keychain in an SSH session) falls back to the file, and the next
//! read or successful write moves the secret into the Keychain and removes the
//! file. The same path carries a file written by an older release into the
//! Keychain on first use.

#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};

/// Keychain service every Cypher secret is filed under. The account is the
/// secret's home file path, which is already unique per data directory.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SERVICE: &str = "ai.mvp-lab.cypher";

/// One credential, named by its home file.
#[derive(Debug, Clone)]
pub struct Secret {
    file: PathBuf,
}

impl Secret {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self { file: file.into() }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Where the secret is kept, for messages to the user.
    pub fn location(&self) -> String {
        if keychain::enabled() {
            "the login Keychain".into()
        } else {
            self.file.display().to_string()
        }
    }

    pub fn read(&self) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(&self.file) {
            Ok(bytes) => {
                if keychain::enabled() && keychain::set(&self.account(), &bytes).is_ok() {
                    remove(&self.file)?;
                }
                Ok(Some(bytes))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if keychain::enabled() {
                    keychain::get(&self.account())
                } else {
                    Ok(None)
                }
            }
            Err(err) => Err(err),
        }
    }

    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if keychain::enabled() && keychain::set(&self.account(), bytes).is_ok() {
            return remove(&self.file);
        }
        write_private(&self.file, bytes)
    }

    pub fn delete(&self) -> io::Result<()> {
        remove(&self.file)?;
        if keychain::enabled() {
            keychain::delete(&self.account())?;
        }
        Ok(())
    }

    fn account(&self) -> String {
        self.file.to_string_lossy().into_owned()
    }
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Replace `path` with `bytes`, readable only by the owner. Written to a
/// sibling first and renamed, so a crash never leaves a half-written secret.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("secret path has no file name"))?
        .to_string_lossy();
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::SERVICE;
    use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement};
    use security_framework::passwords;
    use std::io;
    use std::sync::OnceLock;

    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;

    /// Keychain storage needs a code identity that survives rebuilds and
    /// updates: an Apple-anchored signature. `CYPHER_SECRETS=file` forces the
    /// file (tests, and a way out of a broken keychain).
    pub(super) fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            if std::env::var("CYPHER_SECRETS").is_ok_and(|value| value == "file") {
                return false;
            }
            let Ok(requirement) = "anchor apple generic".parse::<SecRequirement>() else {
                return false;
            };
            SecCode::for_self(Flags::NONE)
                .and_then(|code| code.check_validity(Flags::NONE, &requirement))
                .is_ok()
        })
    }

    pub(super) fn get(account: &str) -> io::Result<Option<Vec<u8>>> {
        match passwords::get_generic_password(SERVICE, account) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.code() == NOT_FOUND => Ok(None),
            Err(err) => Err(io::Error::other(format!("Keychain read failed: {err}"))),
        }
    }

    pub(super) fn set(account: &str, bytes: &[u8]) -> io::Result<()> {
        passwords::set_generic_password(SERVICE, account, bytes)
            .map_err(|err| io::Error::other(format!("Keychain write failed: {err}")))
    }

    pub(super) fn delete(account: &str) -> io::Result<()> {
        match passwords::delete_generic_password(SERVICE, account) {
            Err(err) if err.code() != NOT_FOUND => {
                Err(io::Error::other(format!("Keychain delete failed: {err}")))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    use std::io;

    pub(super) fn enabled() -> bool {
        false
    }

    pub(super) fn get(_: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    pub(super) fn set(_: &str, _: &[u8]) -> io::Result<()> {
        Err(io::Error::other("no keychain on this platform"))
    }

    pub(super) fn delete(_: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // Test binaries are never Apple-signed, so these exercise the file store.
    #[test]
    fn a_secret_round_trips_through_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let secret = Secret::new(dir.path().join("session.json"));
        assert_eq!(secret.read().unwrap(), None);
        secret.write(b"one").unwrap();
        secret.write(b"two").unwrap();
        assert_eq!(secret.read().unwrap().as_deref(), Some(&b"two"[..]));
        let mode = std::fs::metadata(secret.file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        secret.delete().unwrap();
        secret.delete().unwrap();
        assert_eq!(secret.read().unwrap(), None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_wider_existing_file_is_replaced_with_a_private_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        Secret::new(&path).write(b"new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn unsigned_test_binaries_use_the_file() {
        assert!(!keychain::enabled());
        let secret = Secret::new("/tmp/x/session.json");
        assert_eq!(secret.location(), "/tmp/x/session.json");
    }

    /// Exercises the real login Keychain; run by hand with
    /// `cargo test -p cypher-secrets -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let account = format!("/tmp/cypher-secrets-test-{}", std::process::id());
        keychain::set(&account, b"value").unwrap();
        assert_eq!(
            keychain::get(&account).unwrap().as_deref(),
            Some(&b"value"[..])
        );
        keychain::delete(&account).unwrap();
        assert_eq!(keychain::get(&account).unwrap(), None);
        keychain::delete(&account).unwrap();
    }
}
