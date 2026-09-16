use crate::error::ErrorCode;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Direct,
    Http,
    Socks5,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub mode: Mode,
    pub proxy_protocol: crate::connection::Protocol,
    pub upstream_host: String,
    pub upstream_port: u16,
    pub username: String,
    pub listen_port: u16,
    pub https_cache: bool,
    pub cache_limit_gb: u32,
    pub close_to_tray: bool,
    pub autostart: bool,
    pub preferences: crate::preferences::Preferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Direct,
            proxy_protocol: crate::connection::Protocol::Http,
            upstream_host: "127.0.0.1".into(),
            upstream_port: 7890,
            username: String::new(),
            listen_port: 8123,
            https_cache: false,
            cache_limit_gb: 5,
            close_to_tray: true,
            autostart: false,
            preferences: Default::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.listen_port < 1024 {
            bail!(ErrorCode::ListenPortInvalid);
        }
        if !(1..=100).contains(&self.cache_limit_gb) {
            bail!(ErrorCode::CacheLimitInvalid);
        }
        if matches!(self.mode, Mode::Http | Mode::Socks5) {
            if self.upstream_port == 0 {
                bail!(ErrorCode::UpstreamPortInvalid);
            }
            let host = self.upstream_host.trim();
            if host.is_empty()
                || host
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#'))
            {
                bail!(ErrorCode::UpstreamHostInvalid);
            }
            if self.upstream_port == self.listen_port
                && (host.eq_ignore_ascii_case("localhost")
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback()))
            {
                bail!(ErrorCode::ProxyLoop);
            }
        }
        if self.username.len() > 255 {
            bail!(ErrorCode::CredentialLength);
        }
        Ok(())
    }
    pub fn load(root: &Path) -> Result<Self> {
        Ok(crate::config_store::load(root)?.settings)
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        self.validate()?;
        crate::config_store::update(root, |document| document.settings = self.clone())
    }
    pub fn pac_url(&self) -> String {
        format!("http://127.0.0.1:{}/proxy.pac", self.listen_port)
    }
}

pub fn secret_entry(name: &str) -> Result<keyring::Entry> {
    #[cfg(feature = "internal-test")]
    {
        let service = std::env::var("GBF_INTERNAL_TEST_ID")?;
        anyhow::ensure!(
            service.starts_with("cc.lzsony.gbf-power-reborn.test-"),
            "Missing isolated test identity"
        );
        Ok(keyring::Entry::new(&service, name)?)
    }
    #[cfg(not(feature = "internal-test"))]
    Ok(keyring::Entry::new("cc.lzsony.gbf-power-reborn", name)?)
}
pub fn read_password() -> Result<String> {
    match secret_entry("upstream")?.get_password() {
        Ok(value) => Ok(value),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(_) => bail!(ErrorCode::SecretReadFailed),
    }
}
pub fn write_password(value: &str) -> Result<()> {
    let entry = secret_entry("upstream")?;
    if value.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    } else {
        entry
            .set_password(value)
            .context(ErrorCode::SecretWriteFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn rejects_loop_and_bad_ports() {
        let mut s = Settings {
            mode: Mode::Socks5,
            upstream_port: 8123,
            ..Default::default()
        };
        assert!(s.validate().is_err());
        s.upstream_port = 7890;
        assert!(s.validate().is_ok());
        s.listen_port = 80;
        assert!(s.validate().is_err());
    }
    #[test]
    fn settings_round_trip_contains_no_password() {
        let dir = tempfile::tempdir().unwrap();
        Settings::default().save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().listen_port, 8123);
        assert!(!fs::read_to_string(dir.path().join("config.json"))
            .unwrap()
            .contains("password"));
    }
}
