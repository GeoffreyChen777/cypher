//! This installation's device identity: the persisted device id and the
//! human device name for its registry row.

use std::io::Write;
use std::path::Path;

use crate::EngineError;
use crate::util::new_id;

/// Best-effort human name for this device's registry row.
pub(crate) fn local_device_name(device_id: &str) -> String {
    select_local_device_name(
        [
            cypher_env::var("DEVICE_NAME"),
            native_friendly_device_name(),
            std::env::var("HOSTNAME").ok(),
            gethostname::gethostname().into_string().ok(),
            std::fs::read_to_string("/etc/hostname").ok(),
        ],
        device_id,
        std::env::consts::OS,
    )
}

fn select_local_device_name(
    candidates: impl IntoIterator<Item = Option<String>>,
    device_id: &str,
    platform: &str,
) -> String {
    candidates
        .into_iter()
        .flatten()
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let platform = match platform {
                "macos" => "macOS",
                "windows" => "Windows",
                "linux" => "Linux",
                _ => "Local",
            };
            let short_id: String = device_id.chars().take(8).collect();
            format!("{platform} device {short_id}")
        })
}

#[cfg(target_os = "macos")]
fn native_friendly_device_name() -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Stable per-installation device id, persisted at `{data_dir}/device-id`.
pub(crate) fn load_or_create_device_id(data_dir: &Path) -> Result<String, EngineError> {
    std::fs::create_dir_all(data_dir)?;
    // EngineInfo is resolved before the lifetime InstanceLock is acquired, so
    // identity creation and legacy repair need their own short critical section.
    // The OS releases this lock after a crash; unlike a create_new lockfile it
    // cannot strand an installation permanently.
    let _identity_lock = DeviceIdentityLock::acquire(data_dir)?;
    let path = data_dir.join("device-id");
    let recovering_empty = match std::fs::read_to_string(&path) {
        Ok(id) if !id.trim().is_empty() => return Ok(id.trim().to_string()),
        // Older releases used truncate+write. A crash between those operations
        // left a zero-byte file that is safe to replace with a fresh identity.
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };

    let id = new_id();
    let temp_path = data_dir.join(format!(
        ".device-id.tmp-{}-{}",
        std::process::id(),
        new_id()
    ));
    let write_result = (|| -> Result<(), EngineError> {
        let mut temp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        temp.write_all(id.as_bytes())?;
        temp.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    // Fresh installs use create-if-absent. Legacy empty files need an atomic
    // same-directory replacement on Unix; the Windows fallback runs under the
    // identity lock and remains recoverable if interrupted.
    let publish_result = if recovering_empty {
        match std::fs::read_to_string(&path) {
            Ok(id) if !id.trim().is_empty() => {
                let _ = std::fs::remove_file(&temp_path);
                return Ok(id.trim().to_string());
            }
            Ok(_) => replace_empty_device_id(&temp_path, &path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::hard_link(&temp_path, &path)
            }
            Err(err) => Err(err),
        }
    } else {
        std::fs::hard_link(&temp_path, &path)
    };
    let _ = std::fs::remove_file(&temp_path);
    match publish_result {
        Ok(()) => Ok(id),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let winner = std::fs::read_to_string(&path)?;
            if winner.trim().is_empty() {
                Err(EngineError::Other(format!(
                    "invalid device identity {}: file is empty",
                    path.display()
                )))
            } else {
                Ok(winner.trim().to_string())
            }
        }
        Err(err) => Err(err.into()),
    }
}

struct DeviceIdentityLock {
    _file: std::fs::File,
}

impl DeviceIdentityLock {
    fn acquire(data_dir: &Path) -> Result<Self, EngineError> {
        let path = data_dir.join("device-id.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
            let mut retries = 200;
            let file = loop {
                match options.open(&path) {
                    Ok(file) => break file,
                    Err(err)
                        if err.kind() == std::io::ErrorKind::PermissionDenied && retries > 0 =>
                    {
                        retries -= 1;
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(err) => return Err(err.into()),
                }
            };
            return Ok(Self { _file: file });
        }

        #[cfg(not(windows))]
        let file = options.open(&path)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                // SAFETY: flock on a descriptor `file` owns for this call.
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EINTR) {
                    return Err(err.into());
                }
            }
        }

        #[cfg(not(windows))]
        Ok(Self { _file: file })
    }
}

fn replace_empty_device_id(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::rename(temp_path, path)
    }
    #[cfg(not(unix))]
    {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        std::fs::hard_link(temp_path, path)
    }
}

#[cfg(not(target_os = "macos"))]
fn native_friendly_device_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    return std::env::var("COMPUTERNAME").ok();

    #[cfg(not(target_os = "windows"))]
    None
}

#[cfg(test)]
mod device_name_tests {
    use super::select_local_device_name;

    fn name(candidates: &[Option<&str>], device_id: &str, platform: &str) -> String {
        select_local_device_name(
            candidates
                .iter()
                .map(|candidate| candidate.map(str::to_string)),
            device_id,
            platform,
        )
    }

    #[test]
    fn explicit_override_wins_and_is_trimmed() {
        assert_eq!(
            name(
                &[Some("  Studio Mac  "), Some("system-host")],
                "17bc0aa2-rest",
                "macos"
            ),
            "Studio Mac"
        );
    }

    #[test]
    fn native_friendly_name_wins_over_hostnames() {
        assert_eq!(
            name(
                &[
                    None,
                    Some("MacBook Pro de Jose"),
                    None,
                    Some("MacBook-Pro.local"),
                ],
                "17bc0aa2-rest",
                "macos"
            ),
            "MacBook Pro de Jose"
        );
    }

    #[test]
    fn windows_computer_name_is_used_when_present() {
        assert_eq!(
            name(
                &[None, Some("DESKTOP-123"), Some("shell-host")],
                "17bc0aa2-rest",
                "windows"
            ),
            "DESKTOP-123"
        );
    }

    #[test]
    fn blank_candidates_are_ignored() {
        assert_eq!(
            name(
                &[Some("  "), None, Some("\n"), Some("linux-box")],
                "17bc0aa2-rest",
                "linux"
            ),
            "linux-box"
        );
    }

    #[test]
    fn final_fallback_is_platform_specific_and_distinct() {
        assert_eq!(
            name(&[None, Some(" ")], "17bc0aa2-rest", "linux"),
            "Linux device 17bc0aa2"
        );
    }
}
