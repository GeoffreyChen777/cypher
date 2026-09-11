//! Device-local title preferences, shared by RPC and every auto-title run.
//! Stored beside the engine identity, not in a UI/profile/workspace document.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use cypher_proto::TitleModelSettings;

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

    pub fn load(&self) -> anyhow::Result<TitleModelSettings> {
        let path = self.inner.lock().unwrap();
        match std::fs::read(&*path) {
            Ok(bytes) => {
                let settings = serde_json::from_slice(&bytes)?;
                validate(&settings)?;
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(TitleModelSettings::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Serialize writes and publish atomically. An unsuccessful save leaves the
    /// old preference in force; a title snapshots it once before its retries.
    pub fn save(&self, settings: &TitleModelSettings) -> anyhow::Result<()> {
        validate(settings)?;
        let path = self.inner.lock().unwrap();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(settings)?)?;
        std::fs::rename(tmp, &*path)?;
        Ok(())
    }
}

pub fn validate(settings: &TitleModelSettings) -> anyhow::Result<()> {
    if let Some(model) = &settings.model {
        anyhow::ensure!(
            !model.is_empty()
                && model.len() <= 512
                && !model.chars().any(|c| c.is_whitespace() || c.is_control()),
            "Choose a valid model from this device's catalog"
        );
    }
    Ok(())
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
