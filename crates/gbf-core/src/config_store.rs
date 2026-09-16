//! One durable document; every reader/writer uses the same process-wide lock.
//! The desktop's single-instance guard supplies the cross-process exclusion.
use crate::config::Settings;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, io::Write, path::Path, sync::Mutex};

static STORE: Mutex<()> = Mutex::new(());
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    pub schema_version: u32,
    pub settings: Settings,
}

impl Document {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("Unsupported config schemaVersion: {}", self.schema_version);
        }
        self.settings.validate()?;
        Ok(())
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Configuration has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    pending.write_all(bytes)?;
    pending.as_file().sync_all()?;
    // persist replaces atomically, including on Windows; never unlink the old file.
    pending.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn checked_root(root: &Path) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(root) {
        anyhow::ensure!(
            meta.is_dir() && !crate::data_directory::is_link(&meta),
            "Configuration root must be a regular directory"
        );
    }
    let path = root.join("config.json");
    if let Ok(meta) = fs::symlink_metadata(&path) {
        anyhow::ensure!(
            meta.is_file() && !crate::data_directory::is_link(&meta),
            "Configuration must be a regular file"
        );
    }
    Ok(())
}

fn load_unlocked(root: &Path) -> Result<Document> {
    checked_root(root)?;
    let path = root.join("config.json");
    if path.try_exists()? {
        let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
        if value["schemaVersion"].as_u64() != Some(1) {
            bail!("Unsupported config schemaVersion; version 1 is required");
        }
        let document: Document = serde_json::from_value(value)?;
        document.validate()?;
        return Ok(document);
    }
    let document = Document {
        schema_version: 1,
        settings: Settings::default(),
    };
    document.validate()?;
    atomic_write(&path, &serde_json::to_vec_pretty(&document)?)?;
    Ok(document)
}

pub fn load(root: &Path) -> Result<Document> {
    let _guard = STORE
        .lock()
        .map_err(|_| anyhow::anyhow!("Configuration lock poisoned"))?;
    load_unlocked(root)
}

pub fn update(root: &Path, edit: impl FnOnce(&mut Document)) -> Result<()> {
    let _guard = STORE
        .lock()
        .map_err(|_| anyhow::anyhow!("Configuration lock poisoned"))?;
    let mut document = load_unlocked(root)?;
    edit(&mut document);
    document.validate()?;
    atomic_write(
        &root.join("config.json"),
        &serde_json::to_vec_pretty(&document)?,
    )
}

/// Called once, before any background task or proxy can read configuration.
pub fn initialize(root: &Path) -> Result<()> {
    load(root).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_schema_and_unsupported_documents_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(load(root.path()).unwrap().schema_version, 1);
        let path = root.path().join("config.json");
        for bytes in [
            br#"{"schemaVersion":2}"#.as_slice(),
            br#"{"schemaVersion":3}"#,
            b"broken",
        ] {
            fs::write(&path, bytes).unwrap();
            assert!(initialize(root.path()).is_err());
            assert!(Settings::default().save(root.path()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn concurrent_section_updates_do_not_lose_changes() {
        let dir = tempfile::tempdir().unwrap();
        initialize(dir.path()).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for n in 0..30 {
                    update(dir.path(), |d| d.settings.listen_port = 9000 + n).unwrap();
                }
            });
            scope.spawn(|| {
                for n in 0..30 {
                    update(dir.path(), |d| d.settings.upstream_port = 10000 + n).unwrap();
                }
            });
        });
        let document = load(dir.path()).unwrap();
        assert_eq!(document.settings.listen_port, 9029);
        assert_eq!(document.settings.upstream_port, 10029);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[cfg(windows)]
    #[test]
    fn replacement_failure_retains_original_and_cleans_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        initialize(dir.path()).unwrap();
        let path = dir.path().join("config.json");
        let before = fs::read(&path).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(update(dir.path(), |d| d.settings.listen_port = 9000).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        drop(held);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        update(dir.path(), |d| d.settings.listen_port = 9000).unwrap();
    }
}
