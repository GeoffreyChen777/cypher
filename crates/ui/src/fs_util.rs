//! Small filesystem helpers shared by the settings/preference files.

use std::io::Write as _;
use std::path::Path;

/// Write `bytes` to `dir/file_name` atomically: stage into a uniquely named
/// temp file beside it (created with unix `mode`), fsync, then rename over the
/// destination — a crash mid-write never leaves a torn file. On failure the
/// staging file is removed and the destination is left untouched.
pub(crate) fn write_atomic(
    dir: &Path,
    file_name: &str,
    bytes: &[u8],
    mode: u32,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{file_name}-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, dir.join(file_name))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
