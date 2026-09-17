//! Quick-chat scratch folders on THIS host: `<temp>/cypher-scratch/<chat id>`.
//!
//! The folder shape is the whole contract (see `cypher_proto::scratch`):
//! creation mints exactly that path, deletion removes only a path that
//! matches it for the named chat, and a run in a quick chat recreates the
//! folder when the host's temp dir was emptied (Linux `/tmp` at boot).

use std::path::{Path, PathBuf};

use cypher_proto::scratch::{is_scratch_cwd_for, scratch_dir, valid_scratch_chat_id};

fn temp_root() -> PathBuf {
    std::env::temp_dir()
}

/// Create (idempotently) the scratch folder for `chat_id`; returns its path.
pub fn create(chat_id: &str) -> Result<String, String> {
    if !valid_scratch_chat_id(chat_id) {
        return Err("Invalid chat id for a scratch folder.".into());
    }
    let dir = scratch_dir(&temp_root(), chat_id);
    std::fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "Could not create the scratch folder {}: {err}",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir.to_string_lossy().into_owned())
}

/// Remove the scratch folder `path` of `chat_id`. Refuses any path that is
/// not `…/cypher-scratch/<chat id>` (a symlink at that path is unlinked, not
/// followed). `Ok(false)` when nothing was there.
pub fn delete(chat_id: &str, path: &str) -> Result<bool, String> {
    if !valid_scratch_chat_id(chat_id) || !is_scratch_cwd_for(path, chat_id) {
        return Err("Not a scratch folder for this chat; nothing was deleted.".into());
    }
    let path = Path::new(path);
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(format!("Could not inspect {}: {err}", path.display())),
    };
    let result = if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|err| format!("Could not delete {}: {err}", path.display()))?;
    Ok(true)
}

/// Recreate a quick chat's scratch folder before a run spawns in it.
pub fn ensure_for_run(chat_id: &str, cwd: &str) {
    if is_scratch_cwd_for(cwd, chat_id) && !Path::new(cwd).is_dir() {
        if let Err(err) = std::fs::create_dir_all(cwd) {
            tracing::warn!(chat = %chat_id, cwd, error = %err, "could not recreate the scratch folder");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_only_touches_a_matching_scratch_folder() {
        let temp = tempfile::tempdir().unwrap();
        let dir = scratch_dir(temp.path(), "chat-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let path = dir.to_string_lossy().into_owned();
        // Wrong chat, wrong shape, traversal: all refused.
        assert!(delete("chat-2", &path).is_err());
        assert!(delete("chat-1", temp.path().to_str().unwrap()).is_err());
        assert!(delete("chat-1", &format!("{path}/../chat-1")).is_err());
        assert!(dir.is_dir());
        assert_eq!(delete("chat-1", &path).unwrap(), true);
        assert!(!dir.exists());
        assert_eq!(delete("chat-1", &path).unwrap(), false);
        // A symlink in the scratch slot is unlinked, never followed.
        let victim = temp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep"), "x").unwrap();
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, &dir).unwrap();
        assert_eq!(delete("chat-1", &path).unwrap(), true);
        assert!(victim.join("keep").is_file());
        assert!(!dir.exists());
    }

    #[test]
    fn create_rejects_bad_ids_and_ensure_recreates() {
        assert!(create("../x").is_err());
        assert!(create("").is_err());
        let temp = tempfile::tempdir().unwrap();
        let dir = scratch_dir(temp.path(), "chat-9");
        let cwd = dir.to_string_lossy().into_owned();
        ensure_for_run("chat-9", &cwd);
        assert!(dir.is_dir());
        // Non-scratch cwds are left alone.
        let other = temp.path().join("plain");
        ensure_for_run("chat-9", other.to_str().unwrap());
        assert!(!other.exists());
    }
}
