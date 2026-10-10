//! Device-local title preferences, shared by RPC and every auto-title run.
//! Stored beside the engine identity, not in a UI/profile/workspace document.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use cypher_proto::TitleModelSettings;

use crate::EngineError;
use crate::util::lock;

#[derive(Clone)]
pub struct TitleSettingsStore {
    inner: Arc<Mutex<PathBuf>>,
}

impl TitleSettingsStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(data_dir.join("title-settings.json"))),
        }
    }

    pub fn load(&self) -> Result<TitleModelSettings, EngineError> {
        let path = lock(&self.inner);
        match std::fs::read(&*path) {
            Ok(bytes) => {
                let settings = serde_json::from_slice(&bytes).map_err(other)?;
                validate(&settings)?;
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(TitleModelSettings::default())
            }
            Err(error) => Err(other(error)),
        }
    }

    /// Serialize writes and publish atomically. An unsuccessful save leaves the
    /// old preference in force; a title snapshots it once before its retries.
    pub fn save(&self, settings: &TitleModelSettings) -> Result<(), EngineError> {
        validate(settings)?;
        let path = lock(&self.inner);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(settings).map_err(other)?).map_err(other)?;
        std::fs::rename(tmp, &*path).map_err(other)?;
        Ok(())
    }
}

pub fn validate(settings: &TitleModelSettings) -> Result<(), EngineError> {
    if let Some(model) = &settings.model
        && (model.is_empty()
            || model.len() > 512
            || model.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(EngineError::Other(
            "Choose a valid model from this device's catalog".into(),
        ));
    }
    Ok(())
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
    fn title_preferences_persist_and_are_device_local() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let store = TitleSettingsStore::new(a.path());
        assert_eq!(store.load().unwrap(), TitleModelSettings::default());
        let chosen = TitleModelSettings {
            model: Some("provider/model".into()),
        };
        store.save(&chosen).unwrap();
        assert_eq!(TitleSettingsStore::new(a.path()).load().unwrap(), chosen);
        assert_eq!(
            TitleSettingsStore::new(b.path()).load().unwrap().model,
            None
        );
        assert!(
            store
                .save(&TitleModelSettings {
                    model: Some(" ".into())
                })
                .is_err()
        );
        assert_eq!(store.load().unwrap(), chosen);
        store.save(&TitleModelSettings::default()).unwrap();
        assert_eq!(store.load().unwrap().model, None);
    }

    #[test]
    fn corrupt_preferences_do_not_silently_enable_another_model() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("title-settings.json"), b"not json").unwrap();
        assert!(TitleSettingsStore::new(dir.path()).load().is_err());
    }
}
