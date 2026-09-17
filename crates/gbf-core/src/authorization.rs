//! Control authentication: codes only activate; daily requests prove device-key possession.
use crate::{acceleration::Line, config, error::ErrorCode};
use anyhow::{bail, Context, Result};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use serde::{Deserialize, Serialize};
use signature::Signer;
use ssh_key::rand_core::{OsRng, RngCore};
use std::{
    path::{Component, Path, PathBuf},
    sync::RwLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbeddedPublicProfile {
    pub deployment_id: String,
    pub url: String,
    pub ca_pem: String,
}

pub fn valid_deployment_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        && !matches!(
            id,
            "con"
                | "prn"
                | "aux"
                | "nul"
                | "com1"
                | "com2"
                | "com3"
                | "com4"
                | "com5"
                | "com6"
                | "com7"
                | "com8"
                | "com9"
                | "lpt1"
                | "lpt2"
                | "lpt3"
                | "lpt4"
                | "lpt5"
                | "lpt6"
                | "lpt7"
                | "lpt8"
                | "lpt9"
        )
}
impl EmbeddedPublicProfile {
    pub fn validate(&self) -> Result<()> {
        if !valid_deployment_id(&self.deployment_id) {
            bail!(ErrorCode::AuthNotConfigured);
        }
        let url = url::Url::parse(&self.url).context(ErrorCode::AuthNotConfigured)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url.port_or_known_default() == Some(0)
        {
            bail!(ErrorCode::AuthNotConfigured);
        }
        self.certificate_der()?;
        Ok(())
    }
    fn certificate_der(&self) -> Result<Vec<u8>> {
        let text = self.ca_pem.trim();
        if !text.starts_with("-----BEGIN CERTIFICATE-----")
            || !text.ends_with("-----END CERTIFICATE-----")
            || text.contains("PRIVATE KEY")
        {
            bail!(ErrorCode::AuthNotConfigured);
        }
        let blocks = rustls_pemfile::read_all(&mut std::io::Cursor::new(text.as_bytes()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context(ErrorCode::AuthNotConfigured)?;
        match blocks.as_slice() {
            [rustls_pemfile::Item::X509Certificate(cert)] => {
                // PEM decoding alone does not validate the X.509 trust anchor.
                rustls::RootCertStore::empty()
                    .add(cert.clone())
                    .map_err(|_| ErrorCode::AuthNotConfigured)?;
                Ok(cert.as_ref().to_vec())
            }
            _ => bail!(ErrorCode::AuthNotConfigured),
        }
    }
    fn binding(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        self.validate()?;
        let mut hash = Sha256::new();
        hash.update(url::Url::parse(&self.url)?.as_str().as_bytes());
        hash.update([0]);
        hash.update(self.certificate_der()?);
        Ok(format!("{:x}", hash.finalize()))
    }
    pub fn client(&self) -> Result<reqwest::Client> {
        self.validate()?;
        Ok(reqwest::Client::builder()
            .no_proxy()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(self.ca_pem.as_bytes())?)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(5))
            .build()?)
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthConfig {
    pub identity_file: PathBuf,
    pub device_name: String,
    pub registered: bool,
    pub binding: String,
}
impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            identity_file: "ssh/device_ed25519".into(),
            device_name: "Reborn".into(),
            registered: false,
            binding: String::new(),
        }
    }
}
impl AuthConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.identity_file.as_os_str().is_empty()
            || !self
                .identity_file
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            bail!(ErrorCode::AuthNotConfigured);
        }
        Ok(())
    }
}
impl AuthConfig {
    fn for_deployment(id: &str) -> Self {
        Self {
            identity_file: PathBuf::from(format!("deployments/{id}/ssh/device_ed25519")),
            ..Default::default()
        }
    }
    pub(crate) fn validate_for(&self, id: &str) -> Result<()> {
        self.validate()?;
        if !valid_deployment_id(id) || self.identity_file != Self::for_deployment(id).identity_file
        {
            bail!(ErrorCode::AuthNotConfigured);
        }
        Ok(())
    }
}
fn safe_path(root: &Path, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        bail!(ErrorCode::AuthNotConfigured);
    }
    let target = root.join(path);
    if let Some(existing) = target.ancestors().find(|p| p.exists()) {
        if !existing.canonicalize()?.starts_with(root.canonicalize()?) {
            bail!(ErrorCode::AuthNotConfigured);
        }
    }
    if target.exists() && !target.canonicalize()?.starts_with(root.canonicalize()?) {
        bail!(ErrorCode::AuthNotConfigured);
    }
    Ok(target)
}
fn ensure_key(root: &Path, c: &AuthConfig) -> Result<()> {
    let path = safe_path(root, &c.identity_file)?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    #[cfg(windows)]
    crate::windows_permissions::protect_directory(path.parent().unwrap())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )?;
    }
    if !path.exists() {
        let key = ssh_key::PrivateKey::random(&mut OsRng, ssh_key::Algorithm::Ed25519)?;
        key.write_openssh_file(&path, ssh_key::LineEnding::LF)?;
        #[cfg(windows)]
        crate::windows_permissions::protect_new_key(&path)?;
    }
    #[cfg(windows)]
    crate::windows_permissions::verify_key(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&path)?.permissions().mode() & 0o077 != 0 {
            bail!(ErrorCode::SshNotConfigured);
        }
    }
    Ok(())
}
fn clear_managed_hosts(root: &Path, config: &AuthConfig) -> Result<()> {
    let directory = config
        .identity_file
        .parent()
        .ok_or(ErrorCode::AuthNotConfigured)?;
    let directory = safe_path(root, directory)?;
    if !directory.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(id) = name
            .strip_prefix("managed-")
            .and_then(|s| s.strip_suffix(".known_hosts"))
        else {
            continue;
        };
        if !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            && !entry.file_type()?.is_dir()
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn atomic(path: PathBuf, bytes: &[u8]) -> Result<()> {
    crate::config_store::atomic_write(&path, bytes)
}

#[derive(Serialize)]
pub struct Envelope {
    key: String,
    timestamp: i64,
    nonce: String,
    payload: String,
    signature: String,
}
pub fn envelope(
    root: &Path,
    config: &AuthConfig,
    path: &str,
    body: &serde_json::Value,
) -> Result<Envelope> {
    let private = ssh_key::PrivateKey::read_openssh_file(&safe_path(root, &config.identity_file)?)
        .context(ErrorCode::SshNotConfigured)?;
    if private.key_data().ed25519().is_none() {
        bail!(ErrorCode::SshNotConfigured);
    }
    let key = format!(
        "ssh-ed25519 {}",
        STANDARD.encode(private.public_key().to_bytes()?)
    );
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let mut random = [0u8; 24];
    OsRng.fill_bytes(&mut random);
    let nonce = URL_SAFE_NO_PAD.encode(random);
    let payload = STANDARD.encode(serde_json::to_vec(body)?);
    let message = format!("REBORN-REQUEST-V1\n{path}\n{timestamp}\n{nonce}\n{key}\n{payload}");
    let signed: ssh_key::Signature = private
        .try_sign(message.as_bytes())
        .context(ErrorCode::AuthInvalid)?;
    Ok(Envelope {
        key,
        timestamp,
        nonce,
        payload,
        signature: STANDARD.encode(signed.as_bytes()),
    })
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteLine {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub host_key: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub active: bool,
    pub device_id: String,
    pub account_id: String,
    pub epoch: u64,
    pub lines: Vec<RemoteLine>,
}
pub async fn request(
    root: &Path,
    c: &AuthConfig,
    p: &EmbeddedPublicProfile,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value> {
    let e = envelope(root, c, path, &body)?;
    let mut response = p
        .client()?
        .post(format!("{}{path}", p.url.trim_end_matches('/')))
        .json(&e)
        .send()
        .await
        .map_err(|_| ErrorCode::AuthUnavailable)?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ErrorCode::AuthUnavailable)?
    {
        if bytes.len() + chunk.len() > 256 * 1024 {
            bail!(ErrorCode::AuthUnavailable);
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| ErrorCode::AuthUnavailable)?;
    if !status.is_success() {
        bail!(match value["error"].as_str().unwrap_or("") {
            "AUTH_REVOKED" => ErrorCode::AuthRevoked,
            "AUTH_DENIED" => ErrorCode::AuthInvalid,
            "DEVICE_LIMIT" => ErrorCode::AuthDeviceLimit,
            "DEVICE_BOUND" => ErrorCode::AuthDeviceBound,
            _ => ErrorCode::AuthUnavailable,
        });
    }
    Ok(value)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogView {
    pub registered: bool,
    pub code: Option<String>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub configured: bool,
    pub state: AuthPhase,
    pub account_id: Option<String>,
    pub device_id: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthPhase {
    #[default]
    Unactivated,
    Active,
    Revoked,
    Unavailable,
}
struct State {
    view: View,
    reply: Option<Reply>,
    checked: Option<Instant>,
}
pub struct Authorization {
    root: PathBuf,
    profile: Option<EmbeddedPublicProfile>,
    codes: std::sync::Arc<dyn AccessCodeStore>,
    operation: Mutex<()>,
    state: RwLock<State>,
}
trait AccessCodeStore: Send + Sync {
    fn read(&self, deployment: &str) -> Result<String>;
    fn write(&self, deployment: &str, code: &str) -> Result<()>;
    fn clear(&self, deployment: &str) -> Result<()>;
}
struct SystemAccessCodeStore;
impl AccessCodeStore for SystemAccessCodeStore {
    fn read(&self, deployment: &str) -> Result<String> {
        config::secret_entry(&format!("access-code:{deployment}"))?
            .get_password()
            .context(ErrorCode::SecretReadFailed)
    }
    fn write(&self, deployment: &str, code: &str) -> Result<()> {
        config::secret_entry(&format!("access-code:{deployment}"))?
            .set_password(code)
            .context(ErrorCode::SecretWriteFailed)
    }
    fn clear(&self, deployment: &str) -> Result<()> {
        match config::secret_entry(&format!("access-code:{deployment}"))?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).context(ErrorCode::SecretWriteFailed),
        }
    }
}
impl Authorization {
    pub fn new(root: PathBuf, profile: Option<EmbeddedPublicProfile>) -> Result<Self> {
        Self::with_codes(root, profile, std::sync::Arc::new(SystemAccessCodeStore))
    }
    fn with_codes(
        root: PathBuf,
        profile: Option<EmbeddedPublicProfile>,
        codes: std::sync::Arc<dyn AccessCodeStore>,
    ) -> Result<Self> {
        if let Some(profile) = &profile {
            profile.validate()?;
            let document = crate::config_store::load(&root)?;
            let previous = document.authorizations.get(&profile.deployment_id);
            let binding = profile.binding()?;
            let mut config = previous
                .cloned()
                .unwrap_or_else(|| AuthConfig::for_deployment(&profile.deployment_id));
            config.validate_for(&profile.deployment_id)?;
            if previous.is_some() && config.binding != binding {
                codes.clear(&profile.deployment_id)?;
                clear_managed_hosts(&root, &config)?;
                config.registered = false;
            }
            if previous.is_none() || config.binding != binding {
                config.binding = binding;
                crate::config_store::update(&root, |document| {
                    document
                        .authorizations
                        .insert(profile.deployment_id.clone(), config);
                })?;
            }
        }
        Ok(Self {
            root,
            state: RwLock::new(State {
                view: View {
                    configured: profile.is_some(),
                    ..Default::default()
                },
                reply: None,
                checked: None,
            }),
            profile,
            codes,
            operation: Mutex::new(()),
        })
    }
    fn profile(&self) -> Result<&EmbeddedPublicProfile> {
        self.profile
            .as_ref()
            .ok_or_else(|| ErrorCode::AuthNotConfigured.into())
    }
    fn config(&self) -> Result<AuthConfig> {
        let profile = self.profile()?;
        crate::config_store::load(&self.root)?
            .authorizations
            .get(&profile.deployment_id)
            .cloned()
            .ok_or_else(|| ErrorCode::AuthNotConfigured.into())
    }
    fn save_config(&self, config: &AuthConfig) -> Result<()> {
        let profile = self.profile()?;
        config.validate_for(&profile.deployment_id)?;
        crate::config_store::update(&self.root, |document| {
            document
                .authorizations
                .insert(profile.deployment_id.clone(), config.clone());
        })
    }
    pub fn dialog(&self) -> Result<DialogView> {
        self.dialog_with(|| self.codes.read(&self.profile()?.deployment_id))
    }
    fn dialog_with(&self, read_code: impl FnOnce() -> Result<String>) -> Result<DialogView> {
        let config = self.config()?;
        let code = if config.registered {
            Some(read_code()?)
        } else {
            None
        };
        Ok(DialogView {
            registered: config.registered,
            code,
        })
    }
    pub async fn activate_code(&self, code: Option<String>) -> Result<()> {
        let config = self.config()?;
        self.activate(code, config.device_name).await
    }
    pub fn view(&self) -> View {
        self.state.read().unwrap().view.clone()
    }
    fn accept(&self, reply: Reply, _config: &AuthConfig) -> Result<()> {
        if !reply.active {
            let mut state = self.state.write().unwrap();
            state.view.state = AuthPhase::Revoked;
            state.reply = None;
            bail!(ErrorCode::AuthRevoked);
        }
        let mut ids = std::collections::HashSet::new();
        for line in &reply.lines {
            if !ids.insert(line.id.clone()) {
                bail!(ErrorCode::LineConflict);
            }
        }
        let mut s = self.state.write().unwrap();
        s.view.state = AuthPhase::Active;
        s.view.account_id = Some(reply.account_id.clone());
        s.view.device_id = Some(reply.device_id.clone());
        s.reply = Some(reply);
        s.checked = Some(Instant::now());
        Ok(())
    }
    pub async fn refresh(&self) -> Result<()> {
        let _lock = self.operation.lock().await;
        let profile = self.profile()?;
        let config = self.config()?;
        if !config.registered {
            bail!(ErrorCode::AuthRequired);
        }
        match request(
            &self.root,
            &config,
            profile,
            "/v1/status",
            serde_json::json!({}),
        )
        .await
        {
            Ok(value) => self.accept(
                serde_json::from_value(value).context(ErrorCode::AuthUnavailable)?,
                &config,
            ),
            Err(e) => {
                let mut state = self.state.write().unwrap();
                state.view.state = if e.downcast_ref::<ErrorCode>() == Some(&ErrorCode::AuthRevoked)
                {
                    state.reply = None;
                    AuthPhase::Revoked
                } else {
                    AuthPhase::Unavailable
                };
                Err(e)
            }
        }
    }
    pub async fn activate(&self, code: Option<String>, name: String) -> Result<()> {
        let _lock = self.operation.lock().await;
        let profile = self.profile()?;
        let mut config = self.config()?;
        if name.is_empty() || name.len() > 160 || name.chars().any(char::is_control) {
            bail!(ErrorCode::AuthInvalid);
        }
        config.device_name = name;
        ensure_key(&self.root, &config)?;
        let code = match code {
            Some(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => self
                .codes
                .read(&profile.deployment_id)
                .map_err(|_| ErrorCode::AuthRequired)?,
        };
        let value = request(
            &self.root,
            &config,
            profile,
            "/v1/activate",
            serde_json::json!({"code":code,"name":config.device_name}),
        )
        .await?;
        let reply: Reply = serde_json::from_value(value).context(ErrorCode::AuthUnavailable)?;
        self.codes.write(&profile.deployment_id, &code)?;
        config.registered = true;
        self.save_config(&config)?;
        self.accept(reply, &config)
    }
    pub async fn unbind(&self) -> Result<()> {
        let _lock = self.operation.lock().await;
        let p = self.profile()?;
        let mut c = self.config()?;
        request(&self.root, &c, p, "/v1/unbind", serde_json::json!({})).await?;
        c.registered = false;
        self.save_config(&c)?;
        self.codes.clear(&p.deployment_id)?;
        let mut s = self.state.write().unwrap();
        s.reply = None;
        s.checked = None;
        s.view.state = AuthPhase::Unactivated;
        Ok(())
    }
    pub fn available_lines(&self) -> Vec<(String, String)> {
        self.route_snapshot().1
    }
    pub(crate) fn route_snapshot(&self) -> (AuthPhase, Vec<(String, String)>) {
        let state = self.state.read().unwrap();
        let lines = state
            .reply
            .as_ref()
            .map(|r| {
                r.lines
                    .iter()
                    .map(|l| (l.id.clone(), l.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        (state.view.state, lines)
    }
    pub fn line(&self, id: &str) -> Result<Line> {
        let state = self.state.read().unwrap();
        if state.view.state == AuthPhase::Revoked {
            bail!(ErrorCode::AuthRevoked);
        }
        if state
            .checked
            .is_none_or(|t| t.elapsed() > Duration::from_secs(45))
        {
            bail!(ErrorCode::AuthUnavailable)
        }
        let remote = state
            .reply
            .as_ref()
            .and_then(|r| r.lines.iter().find(|l| l.id == id))
            .ok_or(ErrorCode::AuthLineDenied)?;
        if remote.id.is_empty()
            || !remote
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || remote.id.len() > 64
            || remote.username != "reborn"
        {
            bail!(ErrorCode::LineInvalid)
        }
        let public =
            ssh_key::PublicKey::from_openssh(&remote.host_key).context(ErrorCode::LineInvalid)?;
        if public.algorithm() != ssh_key::Algorithm::Ed25519
            || remote.host_key.contains(['\r', '\n'])
        {
            bail!(ErrorCode::LineInvalid)
        }
        let config = self.config()?;
        let directory = config
            .identity_file
            .parent()
            .ok_or(ErrorCode::AuthNotConfigured)?;
        let known = directory.join(format!("managed-{}.known_hosts", remote.id));
        let authority = if remote.port == 22 {
            remote.host.clone()
        } else {
            format!("[{}]:{}", remote.host, remote.port)
        };
        if remote.host.is_empty()
            || remote
                .host
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '#' | '?' | '[' | ']'))
        {
            bail!(ErrorCode::LineInvalid)
        }
        let content = format!("{authority} {}\n", remote.host_key);
        let path = safe_path(&self.root, &known)?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
            atomic(path, content.as_bytes())?
        }
        let ssh_config = safe_path(&self.root, &directory.join("config"))?;
        if !ssh_config.exists() {
            std::fs::write(ssh_config, "# Reborn isolated SSH configuration\n")?
        }
        Ok(Line {
            id: remote.id.clone(),
            name: remote.name.clone(),
            host: remote.host.clone(),
            port: remote.port,
            username: remote.username.clone(),
            identity_file: config.identity_file,
            known_hosts_file: known,
        })
    }
}

#[cfg(test)]
pub(crate) fn test_profile(id: &str) -> EmbeddedPublicProfile {
    EmbeddedPublicProfile {
        deployment_id: id.into(),
        url: "https://control.example.invalid/".into(),
        ca_pem: crate::certificate::Authority::ephemeral().pem(),
    }
}
#[cfg(test)]
impl Authorization {
    pub(crate) fn set_test_lines(&self, lines: Vec<Line>) -> Result<()> {
        let mut config = self.config()?;
        ensure_key(&self.root, &config)?;
        config.registered = true;
        self.save_config(&config)?;
        let key = ssh_key::PrivateKey::read_openssh_file(&self.root.join(&config.identity_file))?;
        self.accept(
            Reply {
                active: true,
                device_id: "test-device".into(),
                account_id: "test-account".into(),
                epoch: 1,
                lines: lines
                    .into_iter()
                    .map(|line| RemoteLine {
                        id: line.id,
                        name: line.name,
                        host: line.host,
                        port: line.port,
                        username: line.username,
                        host_key: key.public_key().to_openssh().unwrap(),
                    })
                    .collect(),
            },
            &config,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use signature::Verifier;
    use std::sync::{Arc, Mutex as StdMutex};
    #[derive(Default)]
    struct MemoryCodes {
        values: StdMutex<std::collections::BTreeMap<String, String>>,
        cleared: StdMutex<Vec<String>>,
    }
    impl AccessCodeStore for MemoryCodes {
        fn read(&self, id: &str) -> Result<String> {
            self.values
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or(ErrorCode::SecretReadFailed.into())
        }
        fn write(&self, id: &str, code: &str) -> Result<()> {
            self.values.lock().unwrap().insert(id.into(), code.into());
            Ok(())
        }
        fn clear(&self, id: &str) -> Result<()> {
            self.values.lock().unwrap().remove(id);
            self.cleared.lock().unwrap().push(id.into());
            Ok(())
        }
    }
    #[test]
    fn profiles_accept_only_public_https_roots_and_safe_deployment_ids() {
        let profile = test_profile("test-service");
        profile.validate().unwrap();
        for url in [
            "http://control.example.invalid/",
            "https://user:password@control.example.invalid/",
            "https://control.example.invalid/api",
            "https://control.example.invalid/?token=x",
            "https://control.example.invalid/#x",
            "https://control.example.invalid:0/",
        ] {
            assert!(EmbeddedPublicProfile {
                url: url.into(),
                ..profile.clone()
            }
            .validate()
            .is_err());
        }
        for id in ["", "../escape", "UPPER", "con", "space name", "a/b"] {
            assert!(EmbeddedPublicProfile {
                deployment_id: id.into(),
                ..profile.clone()
            }
            .validate()
            .is_err());
        }
        let mut bad = profile.clone();
        bad.ca_pem
            .push_str("\n-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----");
        assert!(bad.validate().is_err());
        assert!(EmbeddedPublicProfile {
            ca_pem: "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n".into(),
            ..profile.clone()
        }
        .validate()
        .is_err());
        assert!(EmbeddedPublicProfile {
            ca_pem: String::new(),
            ..profile
        }
        .validate()
        .is_err());
    }
    #[test]
    fn profiles_isolate_codes_keys_and_binding_changes_require_reactivation() {
        let dir = tempfile::tempdir().unwrap();
        let codes = Arc::new(MemoryCodes::default());
        let a = test_profile("service-a");
        let auth =
            Authorization::with_codes(dir.path().into(), Some(a.clone()), codes.clone()).unwrap();
        let mut config = auth.config().unwrap();
        ensure_key(dir.path(), &config).unwrap();
        config.registered = true;
        auth.save_config(&config).unwrap();
        codes.write("service-a", "fixture-code").unwrap();
        let identity = std::fs::read(dir.path().join(&config.identity_file)).unwrap();
        let directory = dir.path().join(config.identity_file.parent().unwrap());
        let managed = directory.join("managed-fixture.known_hosts");
        std::fs::write(&managed, "fixture host pin").unwrap();
        std::fs::write(directory.join("notes.txt"), "preserve").unwrap();
        let reopened =
            Authorization::with_codes(dir.path().into(), Some(a.clone()), codes.clone()).unwrap();
        assert!(reopened.config().unwrap().registered);
        assert_eq!(
            reopened.dialog().unwrap().code.as_deref(),
            Some("fixture-code")
        );
        assert!(codes.cleared.lock().unwrap().is_empty());
        let invalid = EmbeddedPublicProfile {
            ca_pem: "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n".into(),
            ..a.clone()
        };
        assert!(
            Authorization::with_codes(dir.path().into(), Some(invalid), codes.clone()).is_err()
        );
        assert!(reopened.config().unwrap().registered);
        assert_eq!(codes.read("service-a").unwrap(), "fixture-code");
        assert!(codes.cleared.lock().unwrap().is_empty());
        let b = EmbeddedPublicProfile {
            deployment_id: "service-b".into(),
            ..a.clone()
        };
        let other = Authorization::with_codes(dir.path().into(), Some(b), codes.clone()).unwrap();
        let other_config = other.config().unwrap();
        ensure_key(dir.path(), &other_config).unwrap();
        assert!(!other_config.registered);
        assert_ne!(other_config.identity_file, config.identity_file);
        assert_ne!(
            std::fs::read(dir.path().join(&other_config.identity_file)).unwrap(),
            identity
        );
        assert_eq!(codes.read("service-a").unwrap(), "fixture-code");
        let changed = EmbeddedPublicProfile {
            url: "https://other.example.invalid/".into(),
            ..a.clone()
        };
        let rebound =
            Authorization::with_codes(dir.path().into(), Some(changed.clone()), codes.clone())
                .unwrap();
        assert!(!rebound.config().unwrap().registered);
        assert!(rebound.dialog().unwrap().code.is_none());
        assert!(codes.read("service-a").is_err());
        assert!(rebound.available_lines().is_empty());
        assert!(!managed.exists());
        assert_eq!(
            std::fs::read_to_string(directory.join("notes.txt")).unwrap(),
            "preserve"
        );
        assert_eq!(
            std::fs::read(dir.path().join(&config.identity_file)).unwrap(),
            identity
        );
        let mut config = rebound.config().unwrap();
        config.registered = true;
        rebound.save_config(&config).unwrap();
        codes.write("service-a", "replacement-code").unwrap();
        let rotated = EmbeddedPublicProfile {
            ca_pem: test_profile("fixture").ca_pem,
            ..changed
        };
        let rotated =
            Authorization::with_codes(dir.path().into(), Some(rotated), codes.clone()).unwrap();
        assert!(!rotated.config().unwrap().registered);
        assert_eq!(
            std::fs::read(dir.path().join(config.identity_file)).unwrap(),
            identity
        );
        assert!(codes.read("service-a").is_err());
        let document = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(
            !document.contains("fixture-code")
                && !document.contains("replacement-code")
                && !document.contains("PRIVATE KEY")
        );
        assert_eq!(
            crate::config_store::load(dir.path())
                .unwrap()
                .schema_version,
            1
        );
    }
    #[test]
    fn signatures_bind_path_body_and_unique_nonce() {
        let dir = tempfile::tempdir().unwrap();
        let config = AuthConfig::default();
        ensure_key(dir.path(), &config).unwrap();
        let e = envelope(dir.path(), &config, "/v1/status", &serde_json::json!({})).unwrap();
        let other = envelope(dir.path(), &config, "/v1/status", &serde_json::json!({})).unwrap();
        assert_ne!(e.nonce, other.nonce);
        let public = ssh_key::PublicKey::from_openssh(&e.key).unwrap();
        let signature = ssh_key::Signature::new(
            ssh_key::Algorithm::Ed25519,
            STANDARD.decode(&e.signature).unwrap(),
        )
        .unwrap();
        let signed = format!(
            "REBORN-REQUEST-V1\n/v1/status\n{}\n{}\n{}\n{}",
            e.timestamp, e.nonce, e.key, e.payload
        );
        Verifier::verify(&public, signed.as_bytes(), &signature).unwrap();
        assert!(Verifier::verify(
            &public,
            signed.replace("/v1/status", "/v1/activate").as_bytes(),
            &signature
        )
        .is_err());
        assert!(Verifier::verify(
            &public,
            signed.replace(&e.payload, "e30K").as_bytes(),
            &signature
        )
        .is_err());
        assert!(safe_path(dir.path(), Path::new("../outside")).is_err());
        assert!(safe_path(dir.path(), Path::new("/tmp/outside")).is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
            assert!(safe_path(dir.path(), Path::new("escape/new.key")).is_err());
        }
    }
    #[test]
    fn managed_lines_require_fresh_assignment_and_use_deployment_known_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let auth = Authorization::new(dir.path().into(), Some(test_profile("testing"))).unwrap();
        let line = Line {
            id: "gateway-one".into(),
            name: "Fixture".into(),
            host: "127.0.0.1".into(),
            port: 2222,
            username: "reborn".into(),
            identity_file: "unused".into(),
            known_hosts_file: "unused".into(),
        };
        auth.set_test_lines(vec![line.clone()]).unwrap();
        let managed = auth.line("gateway-one").unwrap();
        assert_eq!(managed.port, 2222);
        assert_eq!(
            managed.known_hosts_file,
            PathBuf::from("deployments/testing/ssh/managed-gateway-one.known_hosts")
        );
        assert!(dir.path().join(&managed.known_hosts_file).is_file());
        assert!(auth.set_test_lines(vec![line.clone(), line]).is_err());
        assert!(auth.line("unassigned").is_err());
        auth.state.write().unwrap().checked = Some(Instant::now() - Duration::from_secs(46));
        assert_eq!(
            auth.line("gateway-one")
                .err()
                .unwrap()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::AuthUnavailable)
        );
        auth.state.write().unwrap().view.state = AuthPhase::Revoked;
        assert_eq!(
            auth.line("gateway-one")
                .err()
                .unwrap()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::AuthRevoked)
        );
    }
    #[tokio::test]
    async fn unconfigured_authorization_does_not_create_identity_or_contact_a_control() {
        let dir = tempfile::tempdir().unwrap();
        let auth = Authorization::new(dir.path().into(), None).unwrap();
        assert!(!auth.view().configured);
        assert_eq!(
            auth.refresh()
                .await
                .unwrap_err()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::AuthNotConfigured)
        );
        assert_eq!(
            auth.activate_code(Some("test".into()))
                .await
                .unwrap_err()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::AuthNotConfigured)
        );
        assert!(auth.available_lines().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn local_control_activation_status_and_unbind_use_signed_envelopes() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certified.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der())
                    .into(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let profile = EmbeddedPublicProfile {
            deployment_id: "local-test".into(),
            url: format!("https://localhost:{port}/"),
            ca_pem: certified.cert.pem(),
        };
        let task = tokio::spawn(async move {
            let mut nonces = std::collections::HashSet::new();
            for (index, path) in ["/v1/activate", "/v1/status", "/v1/status", "/v1/unbind"]
                .into_iter()
                .enumerate()
            {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = tokio_rustls::TlsAcceptor::from(Arc::new(server_config.clone()))
                    .accept(stream)
                    .await
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    header.push(stream.read_u8().await.unwrap());
                    assert!(header.len() < 16384);
                }
                let header = String::from_utf8(header).unwrap();
                assert!(header.starts_with(&format!("POST {path} HTTP/1.1")));
                let size: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_string)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut body = vec![0; size];
                stream.read_exact(&mut body).await.unwrap();
                let e: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert!(nonces.insert(e["nonce"].as_str().unwrap().to_string()));
                let public = ssh_key::PublicKey::from_openssh(e["key"].as_str().unwrap()).unwrap();
                let sig = ssh_key::Signature::new(
                    ssh_key::Algorithm::Ed25519,
                    STANDARD.decode(e["signature"].as_str().unwrap()).unwrap(),
                )
                .unwrap();
                let signed = format!(
                    "REBORN-REQUEST-V1\n{path}\n{}\n{}\n{}\n{}",
                    e["timestamp"],
                    e["nonce"].as_str().unwrap(),
                    e["key"].as_str().unwrap(),
                    e["payload"].as_str().unwrap()
                );
                Verifier::verify(&public, signed.as_bytes(), &sig).unwrap();
                let payload: serde_json::Value = serde_json::from_slice(
                    &STANDARD.decode(e["payload"].as_str().unwrap()).unwrap(),
                )
                .unwrap();
                if path == "/v1/activate" {
                    assert_eq!(payload["code"], "fixture-only");
                } else {
                    assert!(payload.as_object().unwrap().is_empty());
                }
                let body = if index == 2 {
                    r#"{"active":false,"deviceId":"device-fixture","accountId":"account-fixture","epoch":2,"lines":[]}"#
                } else {
                    r#"{"active":true,"deviceId":"device-fixture","accountId":"account-fixture","epoch":1,"lines":[]}"#
                };
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                let _ = stream.shutdown().await;
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let codes = Arc::new(MemoryCodes::default());
        let auth =
            Authorization::with_codes(dir.path().into(), Some(profile), codes.clone()).unwrap();
        auth.activate_code(Some("fixture-only".into()))
            .await
            .unwrap();
        assert!(auth.config().unwrap().registered);
        auth.refresh().await.unwrap();
        assert_eq!(auth.view().state, AuthPhase::Active);
        assert!(!serde_json::to_string(&auth.view())
            .unwrap()
            .contains("fixture-only"));
        assert_eq!(
            auth.refresh()
                .await
                .unwrap_err()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::AuthRevoked)
        );
        assert_eq!(auth.view().state, AuthPhase::Revoked);
        assert!(auth.available_lines().is_empty());
        auth.unbind().await.unwrap();
        assert!(!auth.config().unwrap().registered);
        assert!(codes.read("local-test").is_err());
        assert_eq!(auth.view().state, AuthPhase::Unactivated);
        task.await.unwrap();
    }
}
