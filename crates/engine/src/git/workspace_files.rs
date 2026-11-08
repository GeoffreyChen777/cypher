//! Bounded filesystem access anchored to a verified checkout: directory
//! listings, text and byte reads, whole-file text replacement, and the file
//! manager's mutations — create, chunked import, move, copy, and delete (to
//! the host's Trash or for good). No shell, no ambient relative paths, no
//! symlink traversal (including during rename races). Each component is
//! opened relative to a held directory fd.

use base64::Engine as _;
use cypher_proto::{
    WorkspaceDirectory, WorkspaceEntryChanged, WorkspaceFileChunk, WorkspaceFileContent,
    WorkspaceFileEntry, WorkspaceFileWritten,
};
use std::io;
use std::path::Path;

const TEXT_LIMIT: usize = 256 * 1024;
/// The editor never loads more than `TEXT_LIMIT`, so a save can only be
/// somewhat larger than a full read (the user typed into it).
const WRITE_LIMIT: usize = 1024 * 1024;
const ENTRY_LIMIT: usize = 1000;
/// Raw bytes per `ReadWorkspaceFileChunk` slice: its base64 stays well under
/// the relay's 1 MiB frame when the host device is remote.
pub(crate) const CHUNK_BYTES: usize = 384 * 1024;
/// Decoded bytes one `WriteWorkspaceFileChunk` slice may carry.
const WRITE_CHUNK_LIMIT: usize = 512 * 1024;
/// An imported file's ceiling.
const IMPORT_LIMIT: u64 = 1024 * 1024 * 1024;
/// A copy is measured before it starts; larger trees are refused.
const COPY_ENTRY_LIMIT: usize = 50_000;
const COPY_BYTE_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
const DELETE_ENTRY_LIMIT: usize = 500_000;
const DEPTH_LIMIT: usize = 256;
const NAME_LIMIT: usize = 255;
static READ_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
/// Mutations hold their own slots, so a long copy never starves listings.
static MUTATE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// One workspace file operation on a relative `path`.
#[derive(Debug, Clone)]
pub enum Op {
    List,
    ReadText,
    ReplaceText(String),
    ReadChunk { offset: u64 },
    Create { directory: bool },
    WriteChunk { offset: u64, data: String },
    Move { to: String },
    Copy { to: String },
    Delete { permanent: bool },
}

impl Op {
    fn mutates(&self) -> bool {
        !matches!(self, Op::List | Op::ReadText | Op::ReadChunk { .. })
    }
}

/// Run `op` on a blocking worker. Workers are bounded: a caller waits a
/// little for a slot, then gets "busy" — and a timed-out caller cannot start
/// an unbounded number of filesystem workers, since the permit stays with the
/// worker until it actually finishes.
pub async fn run(root: std::path::PathBuf, path: String, op: Op) -> io::Result<serde_json::Value> {
    if let Op::ReplaceText(text) = &op
        && text.len() > WRITE_LIMIT
    {
        return Err(io::Error::other(
            "file is too large to save from the file browser",
        ));
    }
    let slots = if op.mutates() {
        &MUTATE_SLOTS
    } else {
        &READ_SLOTS
    };
    let permit = tokio::time::timeout(std::time::Duration::from_secs(4), slots.acquire())
        .await
        .map_err(|_| io::Error::other("file browser busy; retry"))?
        .map_err(io::Error::other)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let value = match op {
            Op::List => serde_json::to_value(list(&root, &path)?),
            Op::ReadText => serde_json::to_value(text(&root, &path)?),
            Op::ReplaceText(text) => serde_json::to_value(replace(&root, &path, &text)?),
            Op::ReadChunk { offset } => serde_json::to_value(read_chunk(&root, &path, offset)?),
            Op::Create { directory } => serde_json::to_value(create(&root, &path, directory)?),
            Op::WriteChunk { offset, data } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.as_bytes())
                    .map_err(|_| io::Error::other("chunk is not base64"))?;
                serde_json::to_value(write_chunk(&root, &path, offset, &bytes)?)
            }
            Op::Move { to } => serde_json::to_value(move_entry(&root, &path, &to)?),
            Op::Copy { to } => serde_json::to_value(copy_entry(&root, &path, &to)?),
            Op::Delete { permanent } => serde_json::to_value(delete(&root, &path, permanent)?),
        };
        value.map_err(io::Error::other)
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

/// A path a mutation may CREATE: a valid workspace path whose name fits the
/// filesystem.
fn new_entry_path(path: &str) -> io::Result<()> {
    let parts = components(path)?;
    match parts.last() {
        None => Err(io::Error::other("expected a workspace entry")),
        Some(name) if name.len() > NAME_LIMIT => Err(io::Error::other("name is too long")),
        Some(_) => Ok(()),
    }
}

/// `to` lies inside `from`'s tree (moving or copying a folder into itself).
fn within(from: &str, to: &str) -> bool {
    to == from
        || to
            .strip_prefix(from)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// "notes 14.03.27.md" — Finder's name for a second item of the same name in
/// the Trash.
fn trash_name(name: &str, stamp: &str, attempt: usize) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    if attempt == 0 {
        format!("{stem} {stamp}{ext}")
    } else {
        format!("{stem} {stamp} {}{ext}", attempt + 1)
    }
}

fn already_exists() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "an item with that name already exists",
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod anchored {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};

    pub(super) fn cstr(name: &str) -> io::Result<CString> {
        CString::new(name).map_err(io::Error::other)
    }

    fn check(result: libc::c_int) -> io::Result<()> {
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn child(parent: &File, name: &str, directory: bool) -> io::Result<File> {
        child_with(
            parent,
            name,
            libc::O_RDONLY | if directory { libc::O_DIRECTORY } else { 0 },
        )
    }

    /// `openat` one path component below `parent` with `access` (the access
    /// mode plus any of O_DIRECTORY / O_CREAT|O_EXCL); the safety flags —
    /// never follow a link, never block on a FIFO, never leak across exec —
    /// are always added.
    fn child_with(parent: &File, name: &str, access: libc::c_int) -> io::Result<File> {
        child_mode(parent, name, access, 0)
    }

    fn child_mode(
        parent: &File,
        name: &str,
        access: libc::c_int,
        mode: libc::mode_t,
    ) -> io::Result<File> {
        let name = cstr(name)?;
        let flags = access | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        // SAFETY: name is NUL-terminated; parent owns the fd throughout openat.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags,
                libc::c_uint::from(mode),
            )
        };
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

    /// The checkout root's own directory fd. root is canonicalized/authorized
    /// by the RPC resolver; walk even its ancestors without following links
    /// rather than canonicalizing again (which could follow an ancestor
    /// swapped after authorization).
    fn open_root(root: &Path) -> io::Result<File> {
        if !root.is_absolute() {
            return Err(io::Error::other("expected a workspace file"));
        }
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
        Ok(fd)
    }

    fn open_with(
        root: &Path,
        path: &str,
        directory: bool,
        leaf_access: libc::c_int,
    ) -> io::Result<File> {
        let parts = components(path)?;
        if !directory && parts.is_empty() {
            return Err(io::Error::other("expected a workspace file"));
        }
        let mut fd = open_root(root)?;
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

    /// The directory holding `path`'s last component, and that component.
    /// The root itself is never an entry.
    pub(super) fn open_parent<'a>(root: &Path, path: &'a str) -> io::Result<(File, &'a str)> {
        let parts = components(path)?;
        let Some((leaf, dirs)) = parts.split_last() else {
            return Err(io::Error::other("expected a workspace entry"));
        };
        let mut fd = open_root(root)?;
        for part in dirs {
            fd = child(&fd, part, true)?;
        }
        Ok((fd, leaf))
    }

    pub(super) fn stat_at(parent: &File, name: &str) -> io::Result<libc::stat> {
        let name = cstr(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is an open directory, name is NUL-terminated and
        // stat points to writable storage of the right type.
        check(unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        // SAFETY: fstatat returned 0, so it initialized stat.
        Ok(unsafe { stat.assume_init() })
    }

    pub(super) fn kind(stat: &libc::stat) -> libc::mode_t {
        stat.st_mode & libc::S_IFMT
    }

    pub(super) fn modified_ms(stat: &libc::stat) -> i64 {
        stat.st_mtime * 1000 + stat.st_mtime_nsec / 1_000_000
    }

    /// The one entry kinds the file manager touches: folders and regular
    /// files (never links, sockets, devices).
    pub(super) fn entry_kind(parent: &File, name: &str) -> io::Result<libc::mode_t> {
        let mode = kind(&stat_at(parent, name)?);
        if mode == libc::S_IFDIR || mode == libc::S_IFREG {
            Ok(mode)
        } else {
            Err(io::Error::other("not a regular file or folder"))
        }
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

    /// The names in an open directory except `.`/`..`: at most `limit`,
    /// and whether that was all of them. The directory fd is duplicated, so
    /// `dir` stays usable for `*at` calls.
    pub(super) fn names(dir: &File, limit: usize) -> io::Result<(Vec<String>, bool)> {
        // SAFETY: dup returns a new descriptor or -1.
        let fd = unsafe { libc::dup(dir.as_raw_fd()) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is an owned directory descriptor.
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so fd is still ours to close, once.
            unsafe {
                libc::close(fd);
            }
            return Err(error);
        }
        let stream = Directory(raw);
        // SAFETY: the stream is open; a dup shares the offset, so rewind.
        unsafe { libc::rewinddir(stream.0) };
        let mut out = Vec::new();
        loop {
            let entry = read_entry(&stream)?;
            let Some(name) = entry else {
                return Ok((out, true));
            };
            if matches!(name.as_str(), "." | "..") {
                continue;
            }
            if out.len() >= limit {
                return Ok((out, false));
            }
            out.push(name);
        }
    }

    /// One `readdir`; non-UTF-8 names come back lossily (callers that act
    /// on them reopen by name and simply fail on such an entry).
    fn read_entry(dir: &Directory) -> io::Result<Option<String>> {
        // SAFETY: readdir's errno protocol and pointers are confined to this
        // blocking thread; the name is copied before the next call.
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
            return Ok(None);
        }
        // SAFETY: entry is non-null and d_name is NUL-terminated; it stays
        // valid until the next readdir on this stream.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        Ok(Some(name.to_string_lossy().into_owned()))
    }

    pub(super) fn list(root: &Path, path: &str) -> io::Result<WorkspaceDirectory> {
        let fd = open(root, path, true)?.into_raw_fd();
        // SAFETY: fd is an owned directory descriptor.
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so fd is still ours to close, once.
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
            // SAFETY: entry is non-null and d_name is NUL-terminated; it stays
            // valid until the next readdir on this stream.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let Ok(name_text) = name.to_str() else {
                continue;
            };
            if matches!(name_text, "." | "..") || name_text.eq_ignore_ascii_case(".git") {
                continue;
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: dir.0 is an open stream, name is NUL-terminated and stat
            // points to writable storage of the right type.
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
            // SAFETY: fstatat returned 0, so it initialized stat.
            let stat = unsafe { stat.assume_init() };
            let mode = kind(&stat);
            if mode != libc::S_IFDIR && mode != libc::S_IFREG {
                continue;
            }
            entries.push(WorkspaceFileEntry {
                name: name_text.to_owned(),
                is_dir: mode == libc::S_IFDIR,
                size: (mode == libc::S_IFREG).then_some(stat.st_size as u64),
                modified_ms: Some(modified_ms(&stat)),
            });
            if entries.len() >= ENTRY_LIMIT {
                break;
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(WorkspaceDirectory { entries, truncated })
    }

    pub(super) fn create_file(parent: &File, name: &str, mode: libc::mode_t) -> io::Result<File> {
        child_mode(
            parent,
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            mode,
        )
        .map_err(exists_as_conflict)
    }

    pub(super) fn mkdir_at(parent: &File, name: &str, mode: libc::mode_t) -> io::Result<()> {
        let name = cstr(name)?;
        // SAFETY: parent is an open directory and name is NUL-terminated.
        check(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) })
            .map_err(exists_as_conflict)
    }

    pub(super) fn open_dir(parent: &File, name: &str) -> io::Result<File> {
        child(parent, name, true)
    }

    /// Set an entry's permission bits through its own fd. The engine runs
    /// with a private umask (its data dir), so project entries it creates
    /// get their modes set explicitly rather than inheriting that.
    // `mode_t` is u16 on macOS, where the conversion is needed, and u32 on Linux.
    #[allow(clippy::useless_conversion)]
    pub(super) fn chmod(file: &File, mode: libc::mode_t) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(u32::from(mode & 0o7777)))
    }

    /// [`mkdir_at`] with exactly `mode`.
    pub(super) fn mkdir_mode(parent: &File, name: &str, mode: libc::mode_t) -> io::Result<File> {
        mkdir_at(parent, name, 0o700)?;
        let dir = open_dir(parent, name)?;
        chmod(&dir, mode)?;
        Ok(dir)
    }

    pub(super) fn open_file(parent: &File, name: &str) -> io::Result<File> {
        child(parent, name, false)
    }

    pub(super) fn open_file_append(parent: &File, name: &str) -> io::Result<File> {
        child_with(parent, name, libc::O_WRONLY)
    }

    fn exists_as_conflict(error: io::Error) -> io::Error {
        if error.raw_os_error() == Some(libc::EEXIST) {
            already_exists()
        } else {
            error
        }
    }

    /// `renameat` that never replaces an existing destination.
    pub(super) fn rename_exclusive(
        from_dir: &File,
        from: &str,
        to_dir: &File,
        to: &str,
    ) -> io::Result<()> {
        let (from_c, to_c) = (cstr(from)?, cstr(to)?);
        #[cfg(target_os = "macos")]
        // SAFETY: both directories are open and both names NUL-terminated.
        let result = unsafe {
            libc::renameatx_np(
                from_dir.as_raw_fd(),
                from_c.as_ptr(),
                to_dir.as_raw_fd(),
                to_c.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        // SAFETY: both directories are open and both names NUL-terminated.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                from_dir.as_raw_fd(),
                from_c.as_ptr(),
                to_dir.as_raw_fd(),
                to_c.as_ptr(),
                libc::RENAME_NOREPLACE,
            ) as libc::c_int
        };
        match check(result) {
            Err(error)
                if cfg!(target_os = "linux")
                    && matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) =>
            {
                // A filesystem without RENAME_NOREPLACE: check, then rename.
                if stat_at(to_dir, to).is_ok() {
                    return Err(already_exists());
                }
                rename_plain(from_dir, from, to_dir, to)
            }
            other => other.map_err(exists_as_conflict),
        }
    }

    /// Plain `renameat` — only for a case-only rename of one entry, where
    /// the "existing" destination is the source itself.
    pub(super) fn rename_plain(
        from_dir: &File,
        from: &str,
        to_dir: &File,
        to: &str,
    ) -> io::Result<()> {
        let (from_c, to_c) = (cstr(from)?, cstr(to)?);
        // SAFETY: both directories are open and both names NUL-terminated.
        check(unsafe {
            libc::renameat(
                from_dir.as_raw_fd(),
                from_c.as_ptr(),
                to_dir.as_raw_fd(),
                to_c.as_ptr(),
            )
        })
    }

    pub(super) fn unlink_at(dir: &File, name: &str, directory: bool) -> io::Result<()> {
        let name = cstr(name)?;
        let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: dir is open and name NUL-terminated; unlinkat never follows
        // a link in the final component.
        check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flags) })
    }

    /// Copy a symlink as a symlink (its target text, never what it points at).
    pub(super) fn copy_link(src_dir: &File, name: &str, dst_dir: &File) -> io::Result<()> {
        let name_c = cstr(name)?;
        let mut buf = vec![0u8; 4096];
        // SAFETY: buf is writable for its length; name is NUL-terminated.
        let len = unsafe {
            libc::readlinkat(
                src_dir.as_raw_fd(),
                name_c.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if len < 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(len as usize);
        let target = CString::new(buf).map_err(io::Error::other)?;
        // SAFETY: both strings are NUL-terminated; dst_dir is open.
        check(unsafe { libc::symlinkat(target.as_ptr(), dst_dir.as_raw_fd(), name_c.as_ptr()) })
    }

    /// Copy one regular file's bytes and permission bits to a NEW file.
    /// APFS clones it outright where it can.
    pub(super) fn copy_file(
        src_dir: &File,
        name: &str,
        dst_dir: &File,
        dst_name: &str,
        mode: libc::mode_t,
    ) -> io::Result<u64> {
        let src = open_file(src_dir, name)?;
        let meta = src.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::other("not a regular file"));
        }
        #[cfg(target_os = "macos")]
        {
            let dst_c = cstr(dst_name)?;
            // SAFETY: src is an open regular file, dst_dir an open directory,
            // dst_c NUL-terminated. CLONE_NOFOLLOW: never clone through a link.
            let cloned = unsafe {
                libc::fclonefileat(src.as_raw_fd(), dst_dir.as_raw_fd(), dst_c.as_ptr(), 0x0001)
            };
            if cloned == 0 {
                return Ok(meta.len());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Err(already_exists());
            }
        }
        let mut dst = create_file(dst_dir, dst_name, 0o600)?;
        let copied = chmod(&dst, mode & 0o777).and_then(|()| std::io::copy(&mut &src, &mut dst));
        if let Err(error) = copied {
            drop(dst);
            let _ = unlink_at(dst_dir, dst_name, false);
            return Err(error);
        }
        Ok(meta.len())
    }

    /// The Trash directory on this host, opened without following links.
    #[cfg(target_os = "macos")]
    pub(super) fn trash_dir(_root: &Path) -> io::Result<(File, Option<File>)> {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("no home folder"))?;
        let path = std::path::PathBuf::from(home).join(".Trash");
        let path = path
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF-8 home"))?;
        Ok((open_root(Path::new(path))?, None))
    }

    /// XDG: `$XDG_DATA_HOME/Trash/{files,info}` (created on first use).
    #[cfg(target_os = "linux")]
    pub(super) fn trash_dir(_root: &Path) -> io::Result<(File, Option<File>)> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(".local/share"))
            })
            .ok_or_else(|| io::Error::other("no home folder"))?;
        let base = base
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF-8 home"))?;
        let data = open_root(Path::new(base))?;
        let trash = match mkdir_at(&data, "Trash", 0o700) {
            Ok(()) | Err(_) => open_dir(&data, "Trash")?,
        };
        let _ = mkdir_at(&trash, "files", 0o700);
        let _ = mkdir_at(&trash, "info", 0o700);
        Ok((open_dir(&trash, "files")?, Some(open_dir(&trash, "info")?)))
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_chunk(root: &Path, path: &str, offset: u64) -> io::Result<WorkspaceFileChunk> {
    use std::os::unix::fs::FileExt;
    let file = anchored::open(root, path, false)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let bytes = meta.len();
    if offset > bytes {
        return Err(io::Error::other("offset is past the end of the file"));
    }
    let want = ((bytes - offset) as usize).min(CHUNK_BYTES);
    let mut buf = vec![0u8; want];
    let mut filled = 0;
    while filled < want {
        let read = file.read_at(&mut buf[filled..], offset + filled as u64)?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    buf.truncate(filled);
    let next_offset = offset + filled as u64;
    use std::os::unix::fs::MetadataExt;
    Ok(WorkspaceFileChunk {
        data: base64::engine::general_purpose::STANDARD.encode(&buf),
        offset,
        next_offset,
        bytes,
        done: next_offset >= bytes,
        modified_ms: Some(meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000),
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn create(root: &Path, path: &str, directory: bool) -> io::Result<WorkspaceEntryChanged> {
    new_entry_path(path)?;
    let (parent, name) = anchored::open_parent(root, path)?;
    if directory {
        anchored::mkdir_mode(&parent, name, 0o755)?;
    } else {
        anchored::chmod(&anchored::create_file(&parent, name, 0o600)?, 0o644)?;
    }
    Ok(WorkspaceEntryChanged {
        path: path.to_string(),
        bytes: 0,
        trashed: false,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_chunk(
    root: &Path,
    path: &str,
    offset: u64,
    data: &[u8],
) -> io::Result<WorkspaceEntryChanged> {
    use std::io::Write;
    if data.len() > WRITE_CHUNK_LIMIT {
        return Err(io::Error::other("chunk is too large"));
    }
    if offset + data.len() as u64 > IMPORT_LIMIT {
        return Err(io::Error::other("file is too large to import"));
    }
    new_entry_path(path)?;
    let (parent, name) = anchored::open_parent(root, path)?;
    let mut file = if offset == 0 {
        let file = anchored::create_file(&parent, name, 0o600)?;
        anchored::chmod(&file, 0o644)?;
        file
    } else {
        let file = anchored::open_file_append(&parent, name)?;
        let meta = file.metadata()?;
        // Slices land exactly at the end: a stale or replayed slice can
        // never overwrite what is already there.
        if !meta.is_file() || meta.len() != offset {
            return Err(io::Error::other("import out of step; retry"));
        }
        file
    };
    use std::io::Seek;
    file.seek(io::SeekFrom::Start(offset))?;
    file.write_all(data)?;
    Ok(WorkspaceEntryChanged {
        path: path.to_string(),
        bytes: offset + data.len() as u64,
        trashed: false,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn move_entry(root: &Path, from: &str, to: &str) -> io::Result<WorkspaceEntryChanged> {
    new_entry_path(to)?;
    if within(from, to) && from != to {
        return Err(io::Error::other("a folder cannot move into itself"));
    }
    let (from_dir, from_name) = anchored::open_parent(root, from)?;
    let source = anchored::stat_at(&from_dir, from_name)?;
    let mode = anchored::kind(&source);
    if mode != libc::S_IFDIR && mode != libc::S_IFREG {
        return Err(io::Error::other("not a regular file or folder"));
    }
    let (to_dir, to_name) = anchored::open_parent(root, to)?;
    if from == to {
        return Ok(WorkspaceEntryChanged {
            path: to.to_string(),
            bytes: 0,
            trashed: false,
        });
    }
    match anchored::rename_exclusive(&from_dir, from_name, &to_dir, to_name) {
        // A case-only rename on a case-insensitive volume: the "existing"
        // destination is the source itself.
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                && from_name.eq_ignore_ascii_case(to_name)
                && anchored::stat_at(&to_dir, to_name).is_ok_and(|dest| {
                    dest.st_ino == source.st_ino && dest.st_dev == source.st_dev
                }) =>
        {
            anchored::rename_plain(&from_dir, from_name, &to_dir, to_name)?
        }
        other => other?,
    }
    Ok(WorkspaceEntryChanged {
        path: to.to_string(),
        bytes: 0,
        trashed: false,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn copy_entry(root: &Path, from: &str, to: &str) -> io::Result<WorkspaceEntryChanged> {
    use std::fs::File;
    new_entry_path(to)?;
    if within(from, to) {
        return Err(io::Error::other("a folder cannot be copied into itself"));
    }
    let (from_dir, from_name) = anchored::open_parent(root, from)?;
    let mode = anchored::entry_kind(&from_dir, from_name)?;
    let (to_dir, to_name) = anchored::open_parent(root, to)?;
    if anchored::stat_at(&to_dir, to_name).is_ok() {
        return Err(already_exists());
    }

    fn all_names(dir: &File) -> io::Result<Vec<String>> {
        match anchored::names(dir, COPY_ENTRY_LIMIT)? {
            (names, true) => Ok(names),
            (_, false) => Err(io::Error::other("folder is too large to copy here")),
        }
    }

    /// Entries and bytes below `dir` (measured before anything is written).
    fn measure(dir: &File, depth: usize, totals: &mut (usize, u64)) -> io::Result<()> {
        if depth > DEPTH_LIMIT {
            return Err(io::Error::other("folder is nested too deeply to copy"));
        }
        let names = all_names(dir)?;
        for name in names {
            if name.eq_ignore_ascii_case(".git") {
                continue;
            }
            totals.0 += 1;
            if totals.0 > COPY_ENTRY_LIMIT {
                return Err(io::Error::other("folder is too large to copy here"));
            }
            let Ok(stat) = anchored::stat_at(dir, &name) else {
                continue;
            };
            match anchored::kind(&stat) {
                libc::S_IFDIR => measure(&anchored::open_dir(dir, &name)?, depth + 1, totals)?,
                libc::S_IFREG => {
                    totals.1 += stat.st_size as u64;
                    if totals.1 > COPY_BYTE_LIMIT {
                        return Err(io::Error::other("folder is too large to copy here"));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Recreate `src`'s children inside `dst`: folders, regular files, and
    /// links as links. Git metadata and special files are skipped.
    fn copy_tree(src: &File, dst: &File, depth: usize) -> io::Result<()> {
        if depth > DEPTH_LIMIT {
            return Err(io::Error::other("folder is nested too deeply to copy"));
        }
        let names = all_names(src)?;
        for name in names {
            if name.eq_ignore_ascii_case(".git") {
                continue;
            }
            let Ok(stat) = anchored::stat_at(src, &name) else {
                continue;
            };
            match anchored::kind(&stat) {
                libc::S_IFDIR => {
                    let dest = anchored::mkdir_mode(dst, &name, stat.st_mode & 0o777)?;
                    copy_tree(&anchored::open_dir(src, &name)?, &dest, depth + 1)?;
                }
                libc::S_IFREG => {
                    anchored::copy_file(src, &name, dst, &name, stat.st_mode)?;
                }
                libc::S_IFLNK => anchored::copy_link(src, &name, dst)?,
                _ => {}
            }
        }
        Ok(())
    }

    if mode == libc::S_IFREG {
        let stat = anchored::stat_at(&from_dir, from_name)?;
        anchored::copy_file(&from_dir, from_name, &to_dir, to_name, stat.st_mode)?;
    } else {
        let source = anchored::open_dir(&from_dir, from_name)?;
        measure(&source, 0, &mut (0, 0))?;
        let stat = anchored::stat_at(&from_dir, from_name)?;
        let dest = anchored::mkdir_mode(&to_dir, to_name, stat.st_mode & 0o777)?;
        let copied = copy_tree(&source, &dest, 1);
        if let Err(error) = copied {
            // Never leave half a copy behind.
            let _ = remove_tree(&to_dir, to_name, &mut 0, 0);
            return Err(error);
        }
    }
    Ok(WorkspaceEntryChanged {
        path: to.to_string(),
        bytes: 0,
        trashed: false,
    })
}

/// Remove `name` below `dir` and, for a folder, everything inside it —
/// unlinking links themselves, never following them.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remove_tree(
    dir: &std::fs::File,
    name: &str,
    removed: &mut usize,
    depth: usize,
) -> io::Result<()> {
    if depth > DEPTH_LIMIT {
        return Err(io::Error::other("folder is nested too deeply to delete"));
    }
    *removed += 1;
    if *removed > DELETE_ENTRY_LIMIT {
        return Err(io::Error::other("folder is too large to delete here"));
    }
    let stat = anchored::stat_at(dir, name)?;
    if anchored::kind(&stat) != libc::S_IFDIR {
        return anchored::unlink_at(dir, name, false);
    }
    let inner = anchored::open_dir(dir, name)?;
    // Batches, re-read from the start: removing entries mid-`readdir` is
    // allowed but may skip names.
    loop {
        let (batch, _) = anchored::names(&inner, 4096)?;
        if batch.is_empty() {
            break;
        }
        for child in batch {
            remove_tree(&inner, &child, removed, depth + 1)?;
        }
    }
    anchored::unlink_at(dir, name, true)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn delete(root: &Path, path: &str, permanent: bool) -> io::Result<WorkspaceEntryChanged> {
    let (parent, name) = anchored::open_parent(root, path)?;
    anchored::entry_kind(&parent, name)?;
    if permanent {
        remove_tree(&parent, name, &mut 0, 0)?;
        return Ok(WorkspaceEntryChanged {
            path: path.to_string(),
            bytes: 0,
            trashed: false,
        });
    }
    let (trash, info) = anchored::trash_dir(root)
        .map_err(|e| io::Error::other(format!("the Trash is not available ({e})")))?;
    let now = chrono::Local::now();
    let stamp = now.format("%H.%M.%S").to_string();
    for attempt in 0..=20 {
        let target = if attempt == 0 {
            name.to_string()
        } else {
            trash_name(name, &stamp, attempt - 1)
        };
        // XDG wants the `.trashinfo` claimed (exclusively) before the move.
        if let Some(info) = &info {
            let info_name = format!("{target}.trashinfo");
            let mut file = match anchored::create_file(info, &info_name, 0o600) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            use std::io::Write;
            let original = root.join(path);
            write!(
                file,
                "[Trash Info]\nPath={}\nDeletionDate={}\n",
                percent_encode_path(&original.to_string_lossy()),
                now.format("%Y-%m-%dT%H:%M:%S")
            )?;
            match anchored::rename_exclusive(&parent, name, &trash, &target) {
                Ok(()) => {}
                Err(e) => {
                    let _ = anchored::unlink_at(info, &info_name, false);
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        continue;
                    }
                    return Err(trash_error(e));
                }
            }
        } else {
            match anchored::rename_exclusive(&parent, name, &trash, &target) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(trash_error(e)),
            }
        }
        return Ok(WorkspaceEntryChanged {
            path: path.to_string(),
            bytes: 0,
            trashed: true,
        });
    }
    Err(io::Error::other(
        "the Trash has too many items with that name",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn trash_error(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(libc::EXDEV) {
        io::Error::other("this volume has no Trash")
    } else {
        io::Error::other(format!("could not move to the Trash ({error})"))
    }
}

/// XDG `.trashinfo` paths are URL-escaped (RFC 2396), slashes kept.
fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::other(
        "workspace file access is unsupported on this host",
    ))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn list(_: &Path, _: &str) -> io::Result<WorkspaceDirectory> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn text(_: &Path, _: &str) -> io::Result<WorkspaceFileContent> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn replace(_: &Path, _: &str, _: &str) -> io::Result<WorkspaceFileWritten> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read_chunk(_: &Path, _: &str, _: u64) -> io::Result<WorkspaceFileChunk> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn create(_: &Path, _: &str, _: bool) -> io::Result<WorkspaceEntryChanged> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn write_chunk(_: &Path, _: &str, _: u64, _: &[u8]) -> io::Result<WorkspaceEntryChanged> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn move_entry(_: &Path, _: &str, _: &str) -> io::Result<WorkspaceEntryChanged> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn copy_entry(_: &Path, _: &str, _: &str) -> io::Result<WorkspaceEntryChanged> {
    unsupported()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn delete(_: &Path, _: &str, _: bool) -> io::Result<WorkspaceEntryChanged> {
    unsupported()
}

#[cfg(test)]
mod tests;
