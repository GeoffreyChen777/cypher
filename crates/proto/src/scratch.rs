//! Quick chats: sessions that run in a throwaway folder instead of a project.
//!
//! A quick chat is an ordinary project-less chat whose `cwd` is a scratch
//! folder the HOST creates under its temp directory:
//! `<temp>/cypher-scratch/<chat id>`. The folder shape IS the identity — no
//! synced schema field — so every device recognises a quick chat from the
//! row alone, and the host can verify a delete request names a folder it
//! minted before removing anything.

use std::path::{Path, PathBuf};

/// The directory under the host's temp dir that holds every scratch folder.
pub const SCRATCH_ROOT: &str = "cypher-scratch";

/// Chat ids become directory names: only the characters a client-minted
/// UUID uses are accepted, so a hostile id cannot escape the scratch root.
pub fn valid_scratch_chat_id(chat_id: &str) -> bool {
    !chat_id.is_empty()
        && chat_id.len() <= 64
        && chat_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `<temp>/cypher-scratch/<chat id>` for this host.
pub fn scratch_dir(temp: &Path, chat_id: &str) -> PathBuf {
    temp.join(SCRATCH_ROOT).join(chat_id)
}

/// The chat id a scratch cwd was minted for, when `cwd` has the scratch
/// shape (`…/cypher-scratch/<chat id>`, any temp root). `None` otherwise.
pub fn scratch_chat_id(cwd: &str) -> Option<&str> {
    let cwd = cwd.trim_end_matches(['/', '\\']);
    let (parent, id) = cwd.rsplit_once('/')?;
    let (_, root) = parent.rsplit_once('/')?;
    (root == SCRATCH_ROOT && valid_scratch_chat_id(id)).then_some(id)
}

/// Whether `cwd` is the scratch folder of chat `chat_id`.
pub fn is_scratch_cwd_for(cwd: &str, chat_id: &str) -> bool {
    scratch_chat_id(cwd) == Some(chat_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_shape_round_trips_and_rejects_impostors() {
        let dir = scratch_dir(Path::new("/tmp"), "0a1b-2c3d");
        assert_eq!(dir, PathBuf::from("/tmp/cypher-scratch/0a1b-2c3d"));
        assert_eq!(scratch_chat_id(&dir.to_string_lossy()), Some("0a1b-2c3d"));
        assert_eq!(
            scratch_chat_id("/tmp/cypher-scratch/0a1b-2c3d/"),
            Some("0a1b-2c3d")
        );
        assert!(is_scratch_cwd_for(
            "/var/folders/x/T/cypher-scratch/abc",
            "abc"
        ));
        assert!(!is_scratch_cwd_for(
            "/var/folders/x/T/cypher-scratch/abc",
            "abd"
        ));
        for cwd in [
            "~",
            "/home/u/repo",
            "/tmp/cypher-scratch",
            "/tmp/cypher-scratch/",
            "/tmp/cypher-scratch/../etc",
            "/tmp/cypher-scratch/a b",
            "/tmp/not-scratch/abc",
            "cypher-scratch/abc",
        ] {
            assert_eq!(scratch_chat_id(cwd), None, "{cwd}");
        }
        assert!(!valid_scratch_chat_id(""));
        assert!(!valid_scratch_chat_id(".."));
        assert!(!valid_scratch_chat_id("a/b"));
        assert!(valid_scratch_chat_id(
            "5bf43542-a012-452c-b663-35305054272f"
        ));
    }
}
