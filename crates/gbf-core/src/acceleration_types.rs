use crate::error::ErrorCode;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Line {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub identity_file: PathBuf,
    pub known_hosts_file: PathBuf,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineView {
    pub id: String,
    pub name: String,
    pub revision: String,
}

impl Line {
    pub fn revision(&self, root: &Path) -> String {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(serde_json::to_vec(self).unwrap_or_default());
        for file in [&self.identity_file, &self.known_hosts_file] {
            if let Ok(path) = self.file(root, file) {
                if let Ok(bytes) = std::fs::read(path) {
                    hash.update(bytes);
                }
            }
        }
        format!("{:x}", hash.finalize())
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let identifier = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        };
        if !identifier(&self.id)
            || !identifier(&self.username)
            || self.username.starts_with('-')
            || self.name.is_empty()
            || self.name.len() > 160
            || self.name.chars().any(char::is_control)
            || self.port == 0
            || self.host.is_empty()
            || self.host.starts_with('-')
            || !self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
            || self.host.len() > 253
        {
            bail!(ErrorCode::LineInvalid);
        }
        for path in [&self.identity_file, &self.known_hosts_file] {
            if path.as_os_str().is_empty()
                || !path.components().all(|c| matches!(c, Component::Normal(_)))
            {
                bail!(ErrorCode::LineInvalid);
            }
        }
        Ok(())
    }

    pub(crate) fn file(&self, root: &Path, relative: &Path) -> Result<PathBuf> {
        let path = root
            .join(relative)
            .canonicalize()
            .context(ErrorCode::SshNotConfigured)?;
        let root = root.canonicalize().context(ErrorCode::SshNotConfigured)?;
        if !path.starts_with(root) || !path.is_file() {
            bail!(ErrorCode::SshNotConfigured);
        }
        Ok(path)
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccelerationStatus {
    pub state: Phase,
    pub line_id: Option<String>,
    pub error: Option<ErrorCode>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Disconnected,
    Selecting,
    Connecting,
    Connected,
    Reconnecting,
    Error,
}
pub type SharedStatus = Arc<RwLock<AccelerationStatus>>;

pub(crate) fn set_status(
    status: &SharedStatus,
    line: &Line,
    state: Phase,
    error: Option<ErrorCode>,
) {
    *status.write().unwrap() = AccelerationStatus {
        state,
        line_id: Some(line.id.clone()),
        error,
    };
}
