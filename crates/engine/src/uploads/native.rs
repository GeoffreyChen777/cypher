//! Immutable chunks and crash-resumable local attachment receipts.
//! No legacy staging import; an ACK never stands in for durable file bytes.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
};

const CHUNK_LIMIT: usize = 128 * 1024;
const MAX_CHUNKS: u64 = 1024;
const ENCODED_LIMIT: u64 = MAX_BYTES.div_ceil(3) * 4;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Clone)]
struct Chunk {
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    upload: String,
    request_name: String,
    name: String,
    bytes: u64,
    sha256: String,
    chunks: Vec<Chunk>,
    blocks: Vec<Chunk>,
}
fn fail(text: &str) -> EngineError {
    EngineError::Other(text.into())
}
fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn key(id: &str) -> String {
    hex(id.as_bytes())
}
fn private_dir(path: &Path) -> Result<(), EngineError> {
    let missing: Vec<_> = path
        .ancestors()
        .take_while(|p| !p.exists())
        .map(Path::to_path_buf)
        .collect();
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    for directory in missing.iter().rev() {
        if let Some(parent) = directory.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}
fn sync_dir(path: &Path) -> Result<(), EngineError> {
    File::open(path)?.sync_all()?;
    Ok(())
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<(), EngineError> {
    let parent = path.parent().unwrap();
    private_dir(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|e| EngineError::Io(e.error))?;
    sync_dir(parent)
}
fn lock_uploads(uploads: &Uploads) -> Result<File, EngineError> {
    let root = uploads.inner.dir.join("native-v3");
    private_dir(&root)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(root.join("lock"))?;
    file.lock()?;
    Ok(file) // File drop unlocks even across process death.
}
fn receipt_path(root: &Path, id: &str) -> PathBuf {
    root.join("native-v3/receipts")
        .join(format!("{}.json", key(id)))
}
fn receipt(root: &Path, id: &str) -> Result<Option<Receipt>, EngineError> {
    let path = receipt_path(root, id);
    let value = receipt_at(&path)?;
    if value.as_ref().is_some_and(|r| r.upload != id) {
        return Err(fail("Invalid upload receipt"));
    }
    Ok(value)
}
fn receipt_at(path: &Path) -> Result<Option<Receipt>, EngineError> {
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        return Err(fail("Invalid upload receipt"));
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if bytes.len() > 256 * 1024 {
        return Err(fail("Invalid upload receipt"));
    }
    let value: Receipt =
        serde_json::from_slice(&bytes).map_err(|_| fail("Invalid upload receipt"))?;
    if value.version != 3
        || value.bytes > MAX_BYTES
        || value.chunks.is_empty()
        || value.chunks.len() > MAX_CHUNKS as usize
        || value.name != sanitize(&value.request_name)
        || value
            .chunks
            .iter()
            .any(|c| c.bytes > CHUNK_LIMIT as u64 || c.sha256.len() != 64)
        || value.chunks.iter().map(|c| c.bytes).sum::<u64>() > ENCODED_LIMIT
        || value.blocks.len() > MAX_BYTES.div_ceil(READ_CHUNK_BYTES) as usize
        || value
            .blocks
            .iter()
            .any(|c| c.bytes > READ_CHUNK_BYTES || c.sha256.len() != 64)
        || value
            .blocks
            .iter()
            .map(|c| c.bytes)
            .try_fold(0u64, |a, b| a.checked_add(b))
            != Some(value.bytes)
    {
        return Err(fail("Invalid upload receipt"));
    }
    Ok(Some(value))
}
fn parts(dir: &Path) -> Result<Vec<(u64, PathBuf)>, EngineError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut out = vec![];
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("part") {
            continue;
        }
        let seq = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n < MAX_CHUNKS)
            .ok_or_else(|| fail("Invalid chunk index"))?;
        if out.len() >= MAX_CHUNKS as usize {
            return Err(fail("Too many upload chunks"));
        }
        out.push((seq, path));
    }
    out.sort_by_key(|(seq, _)| *seq);
    Ok(out)
}
fn file_digest(path: &Path) -> Result<(u64, String), EngineError> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_BYTES {
            return Err(fail("Upload too large"));
        }
        hash.update(&buf[..n]);
    }
    Ok((size, format!("{:x}", hash.finalize())))
}
pub(super) fn append(
    uploads: &Uploads,
    id: &str,
    encoded: &str,
    seq: Option<u64>,
) -> Result<(), EngineError> {
    let dir = uploads.staging_dir(id)?;
    if encoded.len() > CHUNK_LIMIT {
        return Err(fail("Upload chunk too large"));
    }
    let bytes = encoded.trim().as_bytes();
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b))
    {
        return Err(fail("Upload chunk is not valid base64"));
    }
    let digest = hex(bytes);
    let _guard = lock_uploads(uploads)?;
    if let Some(receipt) = receipt(&uploads.inner.dir, id)? {
        let seq = seq.ok_or_else(|| fail("Committed upload requires a chunk position"))?;
        let chunk = receipt
            .chunks
            .get(seq as usize)
            .ok_or_else(|| fail("Upload chunk conflict"))?;
        if chunk.bytes != bytes.len() as u64 || chunk.sha256 != digest {
            return Err(fail("Upload chunk conflict"));
        }
        return Ok(());
    }
    uploads.sweep();
    private_dir(&dir)?;
    let chunks = parts(&dir)?;
    let at = seq.unwrap_or_else(|| chunks.last().map(|(seq, _)| seq + 1).unwrap_or(0));
    if at >= MAX_CHUNKS {
        return Err(fail("Invalid chunk index"));
    }
    let path = dir.join(format!("{at:06}.part"));
    if path.exists() {
        if std::fs::read(path)? != bytes {
            return Err(fail("Upload chunk conflict"));
        }
        return Ok(());
    }
    if dir.join("commit.json").exists() {
        return Err(fail("Upload commit already started"));
    }
    let mut size = bytes.len() as u64;
    for (_, path) in &chunks {
        size = size
            .checked_add(std::fs::metadata(path)?.len())
            .ok_or_else(|| fail("Upload too large"))?;
    }
    if size > ENCODED_LIMIT {
        return Err(fail("Upload too large"));
    }
    atomic(&path, bytes)
}
pub(super) fn commit(uploads: &Uploads, id: &str, name: &str) -> Result<String, EngineError> {
    let dir = uploads.staging_dir(id)?;
    if name.is_empty() || name.len() > 512 {
        return Err(fail("Invalid upload filename"));
    }
    let _guard = lock_uploads(uploads)?;
    let safe = sanitize(name);
    let objects = uploads.inner.dir.join("native-v3/objects").join(key(id));
    let path = objects.join(&safe);
    if let Some(receipt) = receipt(&uploads.inner.dir, id)? {
        if receipt.request_name != name {
            return Err(fail("Upload commit conflict"));
        }
        let (size, hash) = file_digest(&path)?;
        if size != receipt.bytes || hash != receipt.sha256 {
            return Err(fail("Committed upload is missing or changed"));
        }
        return Ok(path.to_string_lossy().into_owned());
    }
    let chunks = parts(&dir)?;
    if chunks.is_empty() {
        return Err(fail("Unknown or expired upload"));
    }
    private_dir(&objects)?;
    let mut file = tempfile::NamedTempFile::new_in(&objects)?;
    let mut manifest = Vec::new();
    for (expected, (seq, path)) in chunks.iter().enumerate() {
        if *seq != expected as u64 {
            return Err(fail("Upload is missing a chunk"));
        }
        let metadata = std::fs::metadata(path)?;
        if metadata.len() > CHUNK_LIMIT as u64 {
            return Err(fail("Invalid staged chunk"));
        }
        let bytes = std::fs::read(path)?;
        manifest.push(Chunk {
            bytes: bytes.len() as u64,
            sha256: hex(&bytes),
        });
    }
    if manifest.iter().map(|c| c.bytes).sum::<u64>() > ENCODED_LIMIT {
        return Err(fail("Upload too large"));
    }
    let source = VerifiedChunks {
        queued: chunks
            .into_iter()
            .map(|(_, p)| p)
            .zip(manifest.clone())
            .collect::<Vec<_>>()
            .into_iter(),
        current: None,
    };
    let mut decoder = base64::read::DecoderReader::new(source, &BASE64);
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut blocks = Vec::new();
    loop {
        let mut block = vec![0u8; READ_CHUNK_BYTES as usize];
        let mut filled = 0;
        while filled < block.len() {
            let n = decoder
                .read(&mut block[filled..])
                .map_err(|_| fail("Upload is not valid base64 or a staged chunk changed"))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        block.truncate(filled);
        size += filled as u64;
        if size > MAX_BYTES {
            return Err(fail("Upload too large"));
        }
        hash.update(&block);
        file.write_all(&block)?;
        blocks.push(Chunk {
            bytes: filled as u64,
            sha256: hex(&block),
        });
    }
    let mut source = decoder.into_inner();
    if source.read(&mut [0u8; 1])? != 0 {
        return Err(fail("Upload contains trailing base64"));
    }
    let digest = format!("{:x}", hash.finalize());
    file.as_file().sync_all()?;
    let intent = serde_json::to_vec(&(3u8, name, size, &digest))
        .map_err(|_| fail("Invalid upload intent"))?;
    let intent_path = dir.join("commit.json");
    if intent_path.exists() {
        if std::fs::read(&intent_path)? != intent {
            return Err(fail("Upload commit conflict"));
        }
    } else {
        atomic(&intent_path, &intent)?;
    }
    if path.exists() {
        // Crash after file persistence but before receipt: verify, never
        // overwrite/reinvent bytes, then finish the same commit below.
        if file_digest(&path)? != (size, digest.clone()) {
            return Err(fail("Upload commit conflict"));
        }
    } else {
        file.persist_noclobber(&path)
            .map_err(|e| EngineError::Io(e.error))?;
    }
    sync_dir(&objects)?;
    // Persist both directory entries before acknowledging a public seal.
    sync_dir(objects.parent().unwrap())?;
    let record = Receipt {
        version: 3,
        upload: id.into(),
        request_name: name.into(),
        name: safe,
        bytes: size,
        sha256: digest,
        chunks: manifest,
        blocks,
    };
    #[cfg(test)]
    if uploads
        .inner
        .fail_receipt_once
        .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return Err(fail("Injected receipt persistence failure"));
    }
    atomic(
        &receipt_path(&uploads.inner.dir, id),
        &serde_json::to_vec(&record).map_err(|_| fail("Invalid upload receipt"))?,
    )?;
    sync_dir(&uploads.inner.dir.join("native-v3"))?;
    let _ = std::fs::remove_dir_all(dir);
    Ok(path.to_string_lossy().into_owned())
}

/// Verify the complete original chunks covering a requested range before
/// exposing any bytes. Work is bounded by the small range and chunk budget,
/// not the whole attachment on every RPC read.
pub(super) fn read(
    uploads: &Uploads,
    path: &Path,
    start: u64,
    end: u64,
) -> Result<Option<Vec<u8>>, EngineError> {
    use std::io::{Seek, SeekFrom};
    let mut roots = uploads
        .inner
        .read_only_roots
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    roots.push(uploads.inner.dir.clone());
    for root in roots {
        let Ok(objects) = std::fs::canonicalize(root.join("native-v3/objects")) else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(objects) else {
            continue;
        };
        let components: Vec<_> = relative.components().collect();
        if components.len() != 2 {
            return Err(fail("Invalid committed attachment path"));
        }
        let id = components[0]
            .as_os_str()
            .to_str()
            .ok_or_else(|| fail("Invalid upload id"))?;
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(fail("Invalid upload id"));
        }
        let receipt = receipt_at(&root.join("native-v3/receipts").join(format!("{id}.json")))?
            .ok_or_else(|| fail("Attachment is not committed"))?;
        if key(&receipt.upload) != id {
            return Err(fail("Invalid upload receipt"));
        }
        if components[1].as_os_str() != std::ffi::OsStr::new(&receipt.name)
            || end > receipt.bytes
            || start > end
        {
            return Err(fail("Invalid attachment range"));
        }
        let mut file = File::open(path)?;
        if file.metadata()?.len() != receipt.bytes {
            return Err(fail("Committed upload is missing or changed"));
        }
        let mut result = Vec::with_capacity((end - start) as usize);
        let mut offset = 0;
        for chunk in receipt.blocks {
            let next = offset + chunk.bytes;
            if next > start && offset < end {
                let mut bytes = vec![0u8; chunk.bytes as usize];
                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(&mut bytes)?;
                if hex(&bytes) != chunk.sha256 {
                    return Err(fail("Committed upload is missing or changed"));
                }
                result.extend_from_slice(
                    &bytes[(start.saturating_sub(offset)) as usize
                        ..(end.min(next) - offset) as usize],
                );
            }
            offset = next;
            if offset >= end {
                break;
            }
        }
        if result.len() as u64 != end - start {
            return Err(fail("Invalid attachment receipt"));
        }
        return Ok(Some(result));
    }
    Ok(None)
}

struct VerifiedChunks {
    queued: std::vec::IntoIter<(PathBuf, Chunk)>,
    current: Option<(File, Sha256, u64, Chunk)>,
}
impl Read for VerifiedChunks {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        if into.is_empty() {
            return Ok(0);
        }
        loop {
            if self.current.is_none() {
                let Some((path, chunk)) = self.queued.next() else {
                    return Ok(0);
                };
                self.current = Some((File::open(path)?, Sha256::new(), 0, chunk));
            }
            let (file, hash, count, expected) = self.current.as_mut().unwrap();
            let n = file.read(into)?;
            if n != 0 {
                *count += n as u64;
                hash.update(&into[..n]);
                if *count > expected.bytes {
                    return Err(std::io::Error::other("staged chunk changed"));
                }
                if *count == expected.bytes
                    && format!("{:x}", hash.clone().finalize()) != expected.sha256
                {
                    return Err(std::io::Error::other("staged chunk changed"));
                }
                return Ok(n);
            }
            let (_, hash, count, expected) = self.current.take().unwrap();
            if count != expected.bytes || format!("{:x}", hash.finalize()) != expected.sha256 {
                return Err(std::io::Error::other("staged chunk changed"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_chunks_full_ids_and_restarted_commit_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let uploads = Uploads::new(dir.path());
        uploads.append("samepref-one", "aGVsbG8=", Some(0)).unwrap();
        uploads.append("samepref-one", "aGVsbG8=", Some(0)).unwrap();
        assert!(uploads.append("samepref-one", "b3RoZXI=", Some(0)).is_err());
        let first = uploads.commit("samepref-one", "image.png").unwrap();
        let reopened = Uploads::new(dir.path());
        assert_eq!(reopened.commit("samepref-one", "image.png").unwrap(), first);
        reopened
            .append("samepref-one", "aGVsbG8=", Some(0))
            .unwrap();
        assert!(
            reopened
                .append("samepref-one", "b3RoZXI=", Some(0))
                .is_err()
        );
        assert!(reopened.commit("samepref-one", "different.png").is_err());
        reopened
            .append("samepref-two", "b3RoZXI=", Some(0))
            .unwrap();
        let second = reopened.commit("samepref-two", "image.png").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"hello");
        assert_eq!(std::fs::read(&second).unwrap(), b"other");
        reopened.append("Samepref-one", "eA==", Some(0)).unwrap();
        let differently_cased = reopened.commit("Samepref-one", "image.png").unwrap();
        assert_ne!(
            first, differently_cased,
            "case-insensitive filesystems cannot alias upload identities"
        );
        assert_eq!(
            BASE64
                .decode(reopened.read_chunk(&first, 0, &[]).unwrap().data)
                .unwrap(),
            b"hello"
        );
        std::fs::write(&first, b"wrong").unwrap(); // Same-size corruption.
        assert!(reopened.read_chunk(&first, 0, &[]).is_err());
        assert!(reopened.commit("samepref-one", "image.png").is_err());
    }
    #[test]
    fn failed_receipt_does_not_ack_and_can_finish_without_overwriting_file() {
        let dir = tempfile::tempdir().unwrap();
        let uploads = Uploads::new(dir.path());
        uploads.append("upload", "aGVsbG8=", Some(0)).unwrap();
        uploads
            .inner
            .fail_receipt_once
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(uploads.commit("upload", "image.png").is_err());
        let path = uploads
            .dir()
            .join("native-v3/objects")
            .join(key("upload"))
            .join("image.png");
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(
            uploads.read_chunk(path.to_str().unwrap(), 0, &[]).is_err(),
            "unreceipted bytes are unavailable"
        );
        assert!(uploads.commit("upload", "changed.png").is_err());
        assert_eq!(
            uploads.commit("upload", "image.png").unwrap(),
            path.to_string_lossy()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"hello");
    }
    #[test]
    fn out_of_order_bounded_chunks_cross_chunk_reads_and_private_files() {
        let dir = tempfile::tempdir().unwrap();
        let uploads = Uploads::new(dir.path());
        let a = vec![17; 30000];
        let b = vec![83; 30000];
        uploads
            .append("upload", &BASE64.encode(&b), Some(1))
            .unwrap();
        assert!(uploads.commit("upload", "image.png").is_err());
        uploads
            .append("upload", &BASE64.encode(&a), Some(0))
            .unwrap();
        let path = uploads.commit("upload", "image.png").unwrap();
        let mut expected = a;
        expected.extend(&b);
        let first = uploads.read_chunk(&path, 0, &[]).unwrap();
        assert_eq!(BASE64.decode(first.data).unwrap(), expected[..45000]);
        let last = uploads.read_chunk(&path, first.next_offset, &[]).unwrap();
        assert_eq!(BASE64.decode(last.data).unwrap(), expected[45000..]);
        assert!(last.done);
        assert!(uploads.read_chunk(&path, 60001, &[]).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn arbitrary_base64_boundaries_are_lossless_and_trailing_data_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let uploads = Uploads::new(dir.path());
        for (i, chunk) in ["aG", "VsbG", "8="].iter().enumerate() {
            uploads.append("split", chunk, Some(i as u64)).unwrap();
        }
        let path = uploads.commit("split", "image.png").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"hello");
        uploads.append("trailing", "aGk=", Some(0)).unwrap();
        uploads.append("trailing", "eA==", Some(1)).unwrap();
        assert!(uploads.commit("trailing", "image.png").is_err());
    }
    #[test]
    fn independent_handles_serialize_the_same_commit_and_reuse_its_receipt() {
        let dir = tempfile::tempdir().unwrap();
        Uploads::new(dir.path())
            .append("concurrent", "aGk=", Some(0))
            .unwrap();
        let root = dir.path().to_path_buf();
        let other = root.clone();
        let a = std::thread::spawn(move || {
            Uploads::new(&root)
                .commit("concurrent", "image.png")
                .unwrap()
        });
        let b = std::thread::spawn(move || {
            Uploads::new(&other)
                .commit("concurrent", "image.png")
                .unwrap()
        });
        assert_eq!(a.join().unwrap(), b.join().unwrap());
    }
}
