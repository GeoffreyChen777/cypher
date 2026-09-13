//! Uploads — attachment staging on the chat's host device
//! (feature-inventory §3.7 "Uploads"; port of zeron's `uploads.ts`).
//!
//! The UI streams a file as base64 chunks (~60KB, sized for the relay when the
//! target device is remote); immutable encoded chunks and commit intents stage
//! under `{uploads_root}/native-v3/staging/{uploadId}`. `commit` fsyncs the
//! complete file and a content-verified receipt under the full upload ID,
//! then returns the absolute path, which the
//! composer appends to the prompt so the agent can read the file from disk.
//! Attachments live only on the host device — every read proxies through the
//! owning device via `ReadAttachmentChunk`; nothing is mirrored to the edge.
//!
//! `read_chunk` serves transcript images back in 45KB base64 chunks. Path jail:
//! only files under the uploads dir or a workspace-known chat cwd are readable
//! (the RPC layer supplies the cwd roots) — and only supported image types, as
//! in zeron.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Serialize;

use crate::EngineError;
mod native;

/// A pending upload must finish within this window (covers slow mesh links).
const STAGING_TTL: Duration = Duration::from_secs(10 * 60);
/// Hard cap on an assembled file.
const MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Multiple of 3 so independent base64 chunks concatenate losslessly.
const READ_CHUNK_BYTES: u64 = 45_000;

/// `ReadAttachmentChunk` reply.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentChunk {
    pub name: String,
    pub mime_type: String,
    /// Base64 of this chunk's byte range.
    pub data: String,
    pub next_offset: u64,
    pub done: bool,
}

struct UploadsInner {
    #[cfg(test)]
    fail_receipt_once: std::sync::atomic::AtomicBool,
    /// Profile-scoped durable home for new committed attachments.
    dir: PathBuf,
    /// Chunk staging (`{uploads_root}/tmp/{uploadId}/`).
    tmp: PathBuf,
    /// Historical roots accepted for reads only. Writes and staging never use
    /// them. RwLock: a local-profile import adds its source root at runtime so
    /// imported transcripts resolve without an engine restart.
    read_only_roots: std::sync::RwLock<Vec<PathBuf>>,
}

#[derive(Clone)]
pub struct Uploads {
    inner: Arc<UploadsInner>,
}

impl Uploads {
    /// Use the historical device-global uploads directory.
    pub fn new(data_dir: &Path) -> Self {
        Self::from_root(&data_dir.join("uploads"))
    }

    /// Use an already-resolved profile uploads directory.
    pub fn from_root(dir: &Path) -> Self {
        Self::from_root_with_fallback(dir, None)
    }

    /// Use a profile root for all writes and an optional legacy read-only root.
    pub fn from_root_with_fallback(dir: &Path, legacy_read_root: Option<&Path>) -> Self {
        Self {
            inner: Arc::new(UploadsInner {
                #[cfg(test)]
                fail_receipt_once: std::sync::atomic::AtomicBool::new(false),
                tmp: dir.join("native-v3/staging"),
                dir: dir.to_path_buf(),
                read_only_roots: std::sync::RwLock::new(
                    legacy_read_root
                        .into_iter()
                        .map(Path::to_path_buf)
                        .collect(),
                ),
            }),
        }
    }

    /// The durable uploads dir (a path-jail root).
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// Accept `root` for reads from now on (idempotent). Profile import calls
    /// this so transcripts that embed absolute paths under the local profile's
    /// uploads root keep resolving after the switch to a synced profile.
    pub fn add_read_only_root(&self, root: &Path) {
        let mut roots = self
            .inner
            .read_only_roots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !roots.iter().any(|r| r == root) {
            roots.push(root.to_path_buf());
        }
    }

    /// Stage one base64 chunk. Positional (`seq`) writes are IDEMPOTENT: a client
    /// retrying a chunk whose ack was lost must match the same bytes. Changed
    /// retries cannot overwrite a slot. Callers without `seq` append locally.
    pub fn append(&self, upload_id: &str, data: &str, seq: Option<u64>) -> Result<(), EngineError> {
        native::append(self, upload_id, data, seq)
    }

    /// Assemble the staged chunks into a durable file and return its absolute
    /// path.
    pub fn commit(&self, upload_id: &str, file_name: &str) -> Result<String, EngineError> {
        native::commit(self, upload_id, file_name)
    }

    /// Read one 45KB chunk of an attachment. `extra_roots` are the workspace's
    /// known chat cwds — together with the uploads dir they form the path jail.
    pub fn read_chunk(
        &self,
        path: &str,
        offset: u64,
        extra_roots: &[PathBuf],
    ) -> Result<AttachmentChunk, EngineError> {
        use std::io::{Read, Seek};
        let file = self.inspect(path, extra_roots)?;
        let size = file.size;
        if offset > size {
            return Err(EngineError::Other(
                "Attachment offset is ahead of file".into(),
            ));
        }
        let start = offset;
        let next_offset = (start + READ_CHUNK_BYTES).min(size);
        if let Some(bytes) = native::read(self, &file.resolved, start, next_offset)? {
            return Ok(AttachmentChunk {
                name: file.name,
                mime_type: file.mime_type,
                data: BASE64.encode(bytes),
                next_offset,
                done: next_offset >= size,
            });
        }
        // Read ONLY this chunk's byte range — never the whole file per chunk.
        let mut buf = vec![0u8; (next_offset - start) as usize];
        let mut handle = std::fs::File::open(&file.resolved)?;
        handle.seek(std::io::SeekFrom::Start(start))?;
        let mut read = 0usize;
        while read < buf.len() {
            let n = handle.read(&mut buf[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        if read != buf.len() || handle.metadata()?.len() != size {
            return Err(EngineError::Other(
                "Attachment changed while reading".into(),
            ));
        }
        Ok(AttachmentChunk {
            name: file.name,
            mime_type: file.mime_type,
            data: BASE64.encode(&buf),
            next_offset,
            done: next_offset >= size,
        })
    }

    // ── internals ───────────────────────────────────────────────────────────

    fn staging_dir(&self, upload_id: &str) -> Result<PathBuf, EngineError> {
        // The id becomes a directory name — jail it to a safe charset.
        let ok = !upload_id.is_empty()
            && upload_id.len() <= 64
            && upload_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        if !ok {
            return Err(EngineError::Other("Invalid upload id".into()));
        }
        Ok(self.inner.tmp.join(native::key(upload_id)))
    }

    /// Reclaim staging dirs whose newest chunk is older than the TTL (an upload
    /// abandoned mid-stream must not hold up to 32MB forever).
    fn sweep(&self) {
        let Ok(entries) = std::fs::read_dir(&self.inner.tmp) else {
            return;
        };
        for entry in entries.flatten() {
            let newest = std::fs::read_dir(entry.path())
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|f| f.metadata().ok()?.modified().ok())
                .max();
            // `append` creates the directory before writing its first chunk,
            // and parallel uploads leave a short empty window. Judge an
            // empty directory by its own age instead of reclaiming it
            // immediately.
            let newest = newest.or_else(|| entry.metadata().ok()?.modified().ok());
            let expired = match newest {
                Some(at) => at.elapsed().map(|age| age > STAGING_TTL).unwrap_or(false),
                None => false,
            };
            if expired {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    fn inspect(&self, path: &str, extra_roots: &[PathBuf]) -> Result<InspectedFile, EngineError> {
        let outside = || EngineError::Other("Attachment is outside the upload cache".into());
        // Canonicalize BOTH sides so `..` segments and symlinks can't escape.
        let resolved = std::fs::canonicalize(path).map_err(|_| outside())?;
        let read_roots = self
            .inner
            .read_only_roots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let allowed = std::iter::once(&self.inner.dir)
            .chain(read_roots.iter())
            .chain(extra_roots.iter())
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .any(|root| resolved.starts_with(&root) && resolved != root);
        if !allowed {
            return Err(outside());
        }
        let meta = std::fs::metadata(&resolved)?;
        if !meta.is_file() {
            return Err(EngineError::Other("Attachment is not a file".into()));
        }
        if meta.len() > MAX_BYTES {
            return Err(EngineError::Other("Attachment is too large".into()));
        }
        let mime_type = mime_by_ext(&resolved)
            .ok_or_else(|| EngineError::Other("Attachment is not a supported image".into()))?;
        Ok(InspectedFile {
            name: resolved
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "attachment".into()),
            mime_type: mime_type.to_string(),
            size: meta.len(),
            resolved,
        })
    }
}

struct InspectedFile {
    resolved: PathBuf,
    name: String,
    mime_type: String,
    size: u64,
}

fn sanitize(file_name: &str) -> String {
    let base = Path::new(file_name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let tail: String = cleaned
        .chars()
        .rev()
        .take(80)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if tail.is_empty() || tail == "." || tail == ".." {
        "upload".into()
    } else {
        tail
    }
}

fn mime_by_ext(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        "avif" => Some("image/avif"),
        "heic" => Some("image/heic"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_names() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("my photo (1).png"), "my_photo__1_.png");
        assert_eq!(sanitize(""), "upload");
    }

    #[test]
    fn sweep_spares_fresh_empty_staging_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let uploads = Uploads::from_root(dir.path());
        let racing = uploads.staging_dir("upload-racing").unwrap();
        std::fs::create_dir_all(&racing).unwrap();

        uploads
            .append("upload-other", &BASE64.encode(b"hi"), Some(0))
            .unwrap();
        assert!(racing.exists(), "fresh empty staging dir was reclaimed");

        let stale =
            std::time::SystemTime::now() - (STAGING_TTL + std::time::Duration::from_secs(60));
        std::fs::File::open(&racing)
            .unwrap()
            .set_modified(stale)
            .unwrap();
        uploads
            .append("upload-other", &BASE64.encode(b"hi"), Some(0))
            .unwrap();
        assert!(
            !racing.exists(),
            "abandoned empty staging dir must be swept"
        );
    }

    #[test]
    fn commit_assembles_chunks_into_a_durable_file() {
        let dir = tempfile::tempdir().unwrap();
        let uploads = Uploads::from_root(dir.path());
        uploads
            .append("upload-1", &BASE64.encode(b"local"), Some(0))
            .unwrap();

        let path = uploads.commit("upload-1", "image.png").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"local");
        assert!(!dir.path().join("tmp").join("upload-1").exists());
    }
}
