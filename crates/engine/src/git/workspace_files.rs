//! Bounded filesystem access anchored to a verified checkout: directory
//! listings, text reads, and whole-file text replacement of existing regular
//! files. No shell, no ambient relative paths, no symlink traversal (including
//! during rename races). Each component is opened relative to a held
//! directory fd.

use cypher_proto::{
    WorkspaceDirectory, WorkspaceFileContent, WorkspaceFileEntry, WorkspaceFileWritten,
};
use std::io;
use std::path::Path;

const TEXT_LIMIT: usize = 256 * 1024;
/// The editor never loads more than `TEXT_LIMIT`, so a save can only be
/// somewhat larger than a full read (the user typed into it).
const WRITE_LIMIT: usize = 1024 * 1024;
const ENTRY_LIMIT: usize = 1000;
static READ_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub async fn read(
    root: std::path::PathBuf,
    path: String,
    directory: bool,
) -> io::Result<serde_json::Value> {
    let permit = READ_SLOTS
        .try_acquire()
        .map_err(|_| io::Error::other("file browser busy; retry"))?;
    tokio::task::spawn_blocking(move || {
        // A timed-out caller cannot start an unbounded number of filesystem
        // workers: the permit stays here until this worker actually finishes.
        let _permit = permit;
        if directory {
            serde_json::to_value(list(&root, &path)?).map_err(io::Error::other)
        } else {
            serde_json::to_value(text(&root, &path)?).map_err(io::Error::other)
        }
    })
    .await
    .map_err(io::Error::other)?
}

/// Replace the whole text of an existing regular file. Never creates files,
/// never follows links, never writes through a directory or special file.
pub async fn write(
    root: std::path::PathBuf,
    path: String,
    text: String,
) -> io::Result<serde_json::Value> {
    if text.len() > WRITE_LIMIT {
        return Err(io::Error::other(
            "file is too large to save from the file browser",
        ));
    }
    let permit = READ_SLOTS
        .try_acquire()
        .map_err(|_| io::Error::other("file browser busy; retry"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        serde_json::to_value(replace(&root, &path, &text)?).map_err(io::Error::other)
    })
    .await
    .map_err(io::Error::other)?
}

fn components(path: &str) -> io::Result<Vec<&str>> {
    if path.len() > 4096 || path.contains('\0') || path.starts_with('/') {
        return Err(io::Error::other("expected a relative workspace path"));
    }
    if path.is_empty() {
        return Ok(vec![]);
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts
        .iter()
        .any(|p| matches!(*p, "" | "." | "..") || p.eq_ignore_ascii_case(".git"))
    {
        return Err(io::Error::other(
            "path is not available in the file browser",
        ));
    }
    Ok(parts)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod anchored {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};

    fn child(parent: &File, name: &str, directory: bool) -> io::Result<File> {
        child_with(
            parent,
            name,
            libc::O_RDONLY | if directory { libc::O_DIRECTORY } else { 0 },
        )
    }

    /// `openat` one path component below `parent` with `access` (the access
    /// mode plus any of O_DIRECTORY); the safety flags — never follow a link,
    /// never block on a FIFO, never leak across exec — are always added.
    fn child_with(parent: &File, name: &str, access: libc::c_int) -> io::Result<File> {
        let name = CString::new(name).map_err(io::Error::other)?;
        let flags = access | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        // SAFETY: name is NUL-terminated; parent owns the fd throughout openat.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn open(root: &Path, path: &str, directory: bool) -> io::Result<File> {
        open_with(root, path, directory, libc::O_RDONLY)
    }

    /// The write-side twin of [`open`]: the final component opens for
    /// writing (no O_CREAT — the file must already exist; no O_TRUNC — the
    /// caller truncates only after confirming it is a regular file).
    pub(super) fn open_for_write(root: &Path, path: &str) -> io::Result<File> {
        open_with(root, path, false, libc::O_WRONLY)
    }

    fn open_with(
        root: &Path,
        path: &str,
        directory: bool,
        leaf_access: libc::c_int,
    ) -> io::Result<File> {
        let parts = components(path)?;
        if !root.is_absolute() || (!directory && parts.is_empty()) {
            return Err(io::Error::other("expected a workspace file"));
        }
        // root is canonicalized/authorized by the RPC resolver. Walk even
        // its ancestors without following links, rather than canonicalizing
        // again (which could follow an ancestor swapped after authorization).
        let mut fd = File::open("/")?;
        for part in root.components() {
            match part {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(name) => {
                    let name = name
                        .to_str()
                        .ok_or_else(|| io::Error::other("non-UTF-8 path"))?;
                    if name.eq_ignore_ascii_case(".git") {
                        return Err(io::Error::other("Git metadata is not browsable"));
                    }
                    fd = child(&fd, name, true)?;
                }
                _ => return Err(io::Error::other("workspace root is not canonical")),
            }
        }
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            fd = if last && !directory {
                child_with(&fd, part, leaf_access)?
            } else {
                child(&fd, part, true)?
            };
        }
        Ok(fd)
    }

    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: fdopendir transferred ownership to this guard.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    pub(super) fn list(root: &Path, path: &str) -> io::Result<WorkspaceDirectory> {
        let fd = open(root, path, true)?.into_raw_fd();
        // SAFETY: fd is an owned directory descriptor.
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(error);
        }
        let dir = Directory(raw);
        let mut entries = Vec::new();
        let mut truncated = true;
        for _ in 0..20_000 {
            // SAFETY: readdir's errno protocol and pointers are confined to
            // this blocking thread; each name is copied before the next call.
            let entry = unsafe {
                #[cfg(target_os = "macos")]
                {
                    *libc::__error() = 0;
                }
                #[cfg(target_os = "linux")]
                {
                    *libc::__errno_location() = 0;
                }
                libc::readdir(dir.0)
            };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error);
                }
                truncated = false;
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let Ok(name_text) = name.to_str() else {
                continue;
            };
            if matches!(name_text, "." | "..") || name_text.eq_ignore_ascii_case(".git") {
                continue;
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let result = unsafe {
                libc::fstatat(
                    libc::dirfd(dir.0),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result != 0 {
                continue;
            } // removed while enumerating
            let mode = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
            if mode != libc::S_IFDIR && mode != libc::S_IFREG {
                continue;
            }
            entries.push(WorkspaceFileEntry {
                name: name_text.to_owned(),
                is_dir: mode == libc::S_IFDIR,
            });
            if entries.len() >= ENTRY_LIMIT {
                break;
            }
        }
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        Ok(WorkspaceDirectory { entries, truncated })
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn list(root: &Path, path: &str) -> io::Result<WorkspaceDirectory> {
    anchored::list(root, path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn replace(root: &Path, path: &str, text: &str) -> io::Result<WorkspaceFileWritten> {
    use std::io::Write;
    let mut file = anchored::open_for_write(root, path)?;
    // O_NOFOLLOW already refused a link; a FIFO/device would have opened
    // (non-blocking) — refuse to write through anything but a regular file.
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    file.set_len(0)?;
    file.write_all(text.as_bytes())?;
    file.sync_data()?;
    Ok(WorkspaceFileWritten {
        bytes: text.len() as u64,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn text(root: &Path, path: &str) -> io::Result<WorkspaceFileContent> {
    use std::io::Read;
    let file = anchored::open(root, path, false)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    (&file)
        .take((TEXT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if meta.len() != after.len() || meta.modified().ok() != after.modified().ok() {
        return Err(io::Error::other("file changed while reading; refresh"));
    }
    let truncated = bytes.len() > TEXT_LIMIT || meta.len() > bytes.len() as u64;
    bytes.truncate(TEXT_LIMIT);
    let text = if bytes.contains(&0) {
        None
    } else {
        match std::str::from_utf8(&bytes) {
            Ok(s) => Some(s.to_owned()),
            Err(e) if truncated && e.error_len().is_none() => Some(
                std::str::from_utf8(&bytes[..e.valid_up_to()])
                    .map_err(io::Error::other)?
                    .to_owned(),
            ),
            Err(_) => None,
        }
    };
    Ok(WorkspaceFileContent {
        binary: text.is_none(),
        text,
        bytes: meta.len(),
        truncated,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn list(_: &Path, _: &str) -> io::Result<WorkspaceDirectory> {
    Err(io::Error::other(
        "workspace file browsing is unsupported on this host",
    ))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn text(_: &Path, _: &str) -> io::Result<WorkspaceFileContent> {
    Err(io::Error::other(
        "workspace file reading is unsupported on this host",
    ))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn replace(_: &Path, _: &str, _: &str) -> io::Result<WorkspaceFileWritten> {
    Err(io::Error::other(
        "workspace file editing is unsupported on this host",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_are_relative_and_cannot_enter_git_metadata() {
        for path in [
            "/etc/passwd",
            "../secret",
            "a/../x",
            "a/./x",
            "a//x",
            ".git/config",
            ".GIT/config",
            "src/.Git/config",
            "x/.git/y",
            "x\0y",
        ] {
            assert!(components(path).is_err(), "{path:?}");
        }
        assert!(components("").unwrap().is_empty());
        assert_eq!(components("src/main.rs").unwrap(), ["src", "main.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_text_and_listing_reject_links_and_special_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("src/main.rs"), "hello\n世界").unwrap();
        std::fs::write(root.join("binary"), b"x\0y").unwrap();
        std::fs::write(root.join("latin"), [0xff]).unwrap();
        std::fs::write(root.join("large"), vec![b'a'; TEXT_LIMIT + 10]).unwrap();
        std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
        std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("link")).unwrap();
        assert!(text(&root, "escape/passwd").is_err());
        assert!(text(&root, "link").is_err());
        assert!(list(&root, "escape").is_err());
        assert!(text(&root, "src").is_err());
        assert_eq!(
            text(&root, "src/main.rs").unwrap().text.as_deref(),
            Some("hello\n世界")
        );
        assert!(text(&root, "binary").unwrap().binary);
        assert!(text(&root, "latin").unwrap().binary);
        let large = text(&root, "large").unwrap();
        assert!(large.truncated);
        assert_eq!(large.text.unwrap().len(), TEXT_LIMIT);
        let names = list(&root, "").unwrap();
        assert_eq!(names.entries[0].name, "src");
        assert!(
            !names
                .entries
                .iter()
                .any(|e| [".git", "escape", "link"].contains(&e.name.as_str()))
        );
        let fifo = std::ffi::CString::new(root.join("fifo").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(text(&root, "fifo").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replace_rewrites_only_existing_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "a much longer original body\n").unwrap();
        std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("link")).unwrap();
        std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();

        // Shorter text must not leave the old tail behind.
        assert_eq!(
            replace(&root, "src/main.rs", "fn main() {}").unwrap().bytes,
            12
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
        assert_eq!(
            text(&root, "src/main.rs").unwrap().text.as_deref(),
            Some("fn main() {}")
        );

        // No creation, no links, no directories, no escaping the root.
        assert!(replace(&root, "src/new.rs", "x").is_err());
        assert!(!root.join("src/new.rs").exists());
        assert!(replace(&root, "link", "x").is_err());
        assert!(replace(&root, "escape/passwd", "x").is_err());
        assert!(replace(&root, "src", "x").is_err());
        assert!(replace(&root, "", "x").is_err());
        assert!(replace(&root, "../outside", "x").is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
        let fifo = std::ffi::CString::new(root.join("fifo").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(replace(&root, "fifo", "x").is_err());
    }

    #[test]
    fn truncated_utf8_does_not_split_a_character() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut data = vec![b'a'; TEXT_LIMIT - 1];
        data.extend_from_slice("世界".as_bytes());
        std::fs::write(root.join("utf8"), data).unwrap();
        let result = text(&root, "utf8").unwrap();
        assert!(result.truncated && !result.binary);
        assert_eq!(result.text.unwrap().len(), TEXT_LIMIT - 1);
    }

    #[test]
    fn large_directories_are_explicitly_partial() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for i in 0..ENTRY_LIMIT + 1 {
            std::fs::write(root.join(format!("file-{i}")), "").unwrap();
        }
        let listing = list(&root, "").unwrap();
        assert!(listing.truncated);
        assert_eq!(listing.entries.len(), ENTRY_LIMIT);
    }

    #[test]
    fn git_metadata_is_not_browsable_even_if_selected_as_a_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join(".git");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("config"), "metadata").unwrap();
        assert!(list(&root, "").is_err());
        assert!(text(&root, "config").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replacing_an_authorized_directory_with_a_link_does_not_escape() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let root = parent.join("checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "inside").unwrap();
        std::fs::rename(&root, parent.join("old")).unwrap();
        std::os::unix::fs::symlink("/etc", &root).unwrap();
        assert!(text(&root, "passwd").is_err());
        assert!(list(&root, "").is_err());
    }
}
