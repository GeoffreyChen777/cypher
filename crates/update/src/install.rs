//! Install-kind detection.

use super::*;

/// How this binary was installed — decides the update path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// `~/.cypher/app/<ver>/cypher` behind the `current` symlink (curl|sh
    /// installer / a previous `cypher update`).
    Managed { app_root: PathBuf },
    /// Running out of a macOS `.app` bundle.
    MacApp { bundle: PathBuf },
    /// Source build or hand-copied binary — updates are report-only.
    Unmanaged,
}

pub fn detect_install() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return InstallKind::Unmanaged;
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(app_root) = managed_by_layout(&exe) {
        return InstallKind::Managed { app_root };
    }
    detect_install_from(&exe, home.as_deref())
}

pub(super) fn detect_install_from(exe: &Path, home: Option<&Path>) -> InstallKind {
    if let Some(home) = home {
        // `current_exe` resolves the `current` symlink to the versioned dir;
        // installs live under `~/.cypher/app`. HOME itself may be a symlink
        // alias of the canonical path `/proc/self/exe` reports.
        let app_root = home.join(".cypher").join("app");
        let canonical = std::fs::canonicalize(home)
            .map(|home| home.join(".cypher").join("app"))
            .unwrap_or_else(|_| app_root.clone());
        if exe.starts_with(&app_root) || exe.starts_with(&canonical) {
            return InstallKind::Managed { app_root };
        }
    }
    for ancestor in exe.ancestors() {
        if ancestor.extension().is_some_and(|ext| ext == "app")
            && exe.starts_with(ancestor.join("Contents").join("MacOS"))
        {
            return InstallKind::MacApp {
                bundle: ancestor.to_path_buf(),
            };
        }
    }
    InstallKind::Unmanaged
}

/// The installer layout recognised by shape rather than location: the binary
/// sits in `<root>/<version>/` and `<root>/current` is a symlink to that
/// directory. Covers a relocated `.cypher/app`, a HOME the installer saw
/// differently, or a `CYPHER_DATA_DIR`-style custom root.
pub(super) fn managed_by_layout(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let root = dir.parent()?;
    let current = root.join("current");
    if !current.is_symlink() {
        return None;
    }
    let (Ok(link), Ok(here)) = (std::fs::canonicalize(&current), std::fs::canonicalize(dir)) else {
        return None;
    };
    (link == here).then(|| root.to_path_buf())
}

/// A checkout's `target/{debug,release}/cypher`: updates would silently
/// replace a developer's build with a release, so those stay report-only.
pub fn is_source_build(exe: &Path) -> bool {
    exe.ancestors().any(|dir| {
        dir.file_name().is_some_and(|name| name == "target")
            && dir
                .parent()
                .is_some_and(|parent| parent.join("Cargo.toml").is_file())
    })
}
