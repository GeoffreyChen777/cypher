//! Device-local context recovery settings, shared by RPC and the recovery
//! service. Stored beside the engine identity, like the title preferences.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use cypher_proto::ContextRecoverySettings;

use crate::EngineError;
use crate::util::lock;

#[derive(Clone)]
pub struct ContextRecoverySettingsStore {
    inner: Arc<Mutex<PathBuf>>,
}

impl ContextRecoverySettingsStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(data_dir.join("context-recovery.json"))),
        }
    }

    /// A missing file reads as the defaults, which watch no chat. A corrupt
    /// one is an error rather than the defaults: guessing would silently
    /// stop recovering chats the user enabled.
    pub fn load(&self) -> Result<ContextRecoverySettings, EngineError> {
        let path = lock(&self.inner);
        match std::fs::read(&*path) {
            Ok(bytes) => {
                let settings: ContextRecoverySettings =
                    serde_json::from_slice(&bytes).map_err(other)?;
                settings.validate().map_err(EngineError::Other)?;
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ContextRecoverySettings::default())
            }
            Err(error) => Err(other(error)),
        }
    }

    /// Serialize writes and publish atomically; a failed save leaves the old
    /// settings in force.
    pub fn save(&self, settings: &ContextRecoverySettings) -> Result<(), EngineError> {
        settings.validate().map_err(EngineError::Other)?;
        let path = lock(&self.inner);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(other)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(settings).map_err(other)?).map_err(other)?;
        std::fs::rename(tmp, &*path).map_err(other)?;
        Ok(())
    }
}

/// These errors reach the Settings UI verbatim, so they carry the underlying
/// message without the `io:` prefix [`EngineError::Io`] would add.
fn other(err: impl std::fmt::Display) -> EngineError {
    EngineError::Other(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_persist_per_device_and_start_inert() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let store = ContextRecoverySettingsStore::new(a.path());
        let fresh = store.load().unwrap();
        assert_eq!(fresh, ContextRecoverySettings::default());
        assert!(!fresh.enabled_for(Some("provider/model")));

        let chosen = ContextRecoverySettings {
            triggers: vec!["I can't help".into()],
            model: "provider/fixer".into(),
            enabled_models: vec!["provider/model".into()],
            max_attempts: 2,
            ..Default::default()
        };
        store.save(&chosen).unwrap();
        assert_eq!(
            ContextRecoverySettingsStore::new(a.path()).load().unwrap(),
            chosen
        );
        assert_eq!(
            ContextRecoverySettingsStore::new(b.path()).load().unwrap(),
            ContextRecoverySettings::default()
        );
        let invalid = ContextRecoverySettings {
            max_attempts: 0,
            ..chosen.clone()
        };
        assert!(store.save(&invalid).is_err());
        assert_eq!(store.load().unwrap(), chosen);
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("context-recovery.json"), b"not json").unwrap();
        assert!(
            ContextRecoverySettingsStore::new(dir.path())
                .load()
                .is_err()
        );
    }
}
