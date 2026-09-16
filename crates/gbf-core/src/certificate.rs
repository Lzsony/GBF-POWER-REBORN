use crate::config::secret_entry;
use crate::error::ErrorCode;
use anyhow::{bail, Context, Result};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
    KeyUsagePurpose,
};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateStatus {
    pub exists: bool,
    pub trusted: bool,
    pub fingerprint: Option<String>,
}

trait CaSecret {
    fn read(&self) -> Result<Option<String>>;
    fn write(&self, value: &str) -> Result<()>;
    fn delete(&self) -> Result<()>;
}
impl CaSecret for keyring::Entry {
    fn read(&self) -> Result<Option<String>> {
        match self.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context(ErrorCode::SecretReadFailed),
        }
    }
    fn write(&self, value: &str) -> Result<()> {
        self.set_password(value)
            .context(ErrorCode::SecretWriteFailed)
    }
    fn delete(&self) -> Result<()> {
        match self.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context(ErrorCode::SecretWriteFailed),
        }
    }
}
fn restore_secret(secret: &impl CaSecret, old: Option<&str>) -> Result<()> {
    match old {
        Some(v) => secret.write(v),
        None => secret.delete(),
    }
}
fn save_pair(
    secret: &impl CaSecret,
    key: &str,
    persist: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let old = secret.read()?;
    secret.write(key)?;
    if let Err(error) = persist() {
        restore_secret(secret, old.as_deref())
            .context("CA file write and secret rollback failed")?;
        return Err(error);
    }
    Ok(())
}
fn remove_pair(secret: &impl CaSecret, remove: impl FnOnce() -> Result<()>) -> Result<()> {
    let old = secret.read()?;
    secret.delete()?;
    if let Err(error) = remove() {
        restore_secret(secret, old.as_deref())
            .context("CA file removal and secret rollback failed")?;
        return Err(error);
    }
    Ok(())
}

pub struct Authority {
    certificate: Certificate,
    key: KeyPair,
}
impl Authority {
    #[cfg(test)]
    pub(crate) fn ephemeral() -> Self {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();
        Self { certificate, key }
    }
    #[cfg(test)]
    pub(crate) fn pem(&self) -> String {
        self.certificate.pem()
    }
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_with_entry(root, &secret_entry("local-ca")?)
    }
    fn load_with_entry(root: &Path, entry: &keyring::Entry) -> Result<Self> {
        let pem = fs::read_to_string(cert_path(root)).context(ErrorCode::CertificateMissing)?;
        let key = KeyPair::from_pem(&entry.get_password().context(ErrorCode::SecretReadFailed)?)?;
        let params = CertificateParams::from_ca_cert_pem(&pem)?;
        let certificate = params.self_signed(&key)?;
        Ok(Self { certificate, key })
    }
    pub fn create(root: &Path) -> Result<()> {
        Self::create_with_entry(root, &secret_entry("local-ca")?)
    }
    fn create_with_entry(root: &Path, entry: &keyring::Entry) -> Result<()> {
        if cert_path(root).exists() {
            Self::load_with_entry(root, entry)?;
            return Ok(());
        }
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "GBF Power Reborn Local CA");
        params.distinguished_name = dn;
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(3650);
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        fs::create_dir_all(root)?;
        save_pair(entry, &key.serialize_pem(), || {
            crate::config_store::atomic_write(&cert_path(root), cert.pem().as_bytes())
        })?;
        Ok(())
    }
    pub fn server(&self, host: &str) -> Result<Arc<rustls::ServerConfig>> {
        if !crate::rules::is_asset(host) {
            bail!("此主機不允許 HTTPS 解密");
        }
        let mut params = CertificateParams::new(vec![host.into()])?;
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(90);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate()?;
        let cert = params.signed_by(&key, &self.certificate, &self.key)?;
        let mut server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )?;
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(server))
    }
}
pub fn cert_path(root: &Path) -> PathBuf {
    root.join("local-ca.pem")
}
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
const MUTATION_TIMEOUT: Duration = Duration::from_secs(120);

// Own the child until it has exited, including errors and unwinding. On Windows
// the Job owns its entire tree; on macOS security is a directly owned child.
struct CommandChild {
    #[cfg(windows)]
    child: crate::windows_process::Child,
    #[cfg(not(windows))]
    child: std::process::Child,
    reaped: bool,
}
impl CommandChild {
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        #[cfg(windows)]
        let child = {
            command.env_remove("PSModulePath");
            crate::windows_process::Child::spawn_quiet(command)?
        };
        #[cfg(not(windows))]
        let child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(Self {
            child,
            reaped: false,
        })
    }
    fn terminate_and_wait(&mut self) -> std::io::Result<()> {
        #[cfg(windows)]
        self.child.terminate_and_wait()?;
        #[cfg(not(windows))]
        {
            if self.child.try_wait()?.is_none() {
                self.child.kill()?;
            }
            self.child.wait()?;
        }
        self.reaped = true;
        Ok(())
    }
}
impl Drop for CommandChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.terminate_and_wait();
        }
    }
}
fn quiet(command: &mut Command, timeout: Duration) -> Result<bool> {
    let mut child = CommandChild::spawn(command)?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.child.try_wait()? {
            child.reaped = true;
            return Ok(status.success());
        }
        if started.elapsed() >= timeout {
            child
                .terminate_and_wait()
                .context(ErrorCode::CertificateOperationTimeout)?;
            bail!(ErrorCode::CertificateOperationTimeout);
        }
        std::thread::sleep(Duration::from_millis(20).min(timeout - started.elapsed().min(timeout)));
    }
}
pub fn status(root: &Path) -> CertificateStatus {
    checked_status(root).unwrap_or_else(|_| local_status(root))
}
fn local_status(root: &Path) -> CertificateStatus {
    let path = cert_path(root);
    let exists = path.exists();
    let fingerprint = fs::read(&path)
        .ok()
        .and_then(|pem| rustls_pemfile::certs(&mut pem.as_slice()).next()?.ok())
        .map(|der| crate::cache::digest(&der));
    CertificateStatus {
        exists,
        trusted: false,
        fingerprint,
    }
}
pub fn checked_status(root: &Path) -> Result<CertificateStatus> {
    let mut current = local_status(root);
    current.trusted = current.exists && verify(&cert_path(root))?;
    Ok(current)
}
#[cfg(target_os = "macos")]
fn verify(path: &Path) -> Result<bool> {
    quiet(
        Command::new("/usr/bin/security")
            .arg("verify-cert")
            .arg("-c")
            .arg(path)
            .args(["-p", "basic"]),
        VERIFY_TIMEOUT,
    )
}
#[cfg(windows)]
fn verify(path: &Path) -> Result<bool> {
    quiet(Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $c=New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($env:GBF_CA_PATH); $s=New-Object System.Security.Cryptography.X509Certificates.X509Store('Root','CurrentUser'); try { $s.Open('ReadOnly'); foreach($item in $s.Certificates.Find('FindByThumbprint',$c.Thumbprint,$false)){if([Convert]::ToBase64String($item.RawData) -eq [Convert]::ToBase64String($c.RawData)){exit 0}}; exit 1 } finally { $s.Close() }"]).env("GBF_CA_PATH", path), VERIFY_TIMEOUT)
}
#[cfg(not(any(target_os = "macos", windows)))]
fn verify(_: &Path) -> Result<bool> {
    Ok(false)
}

// A command can change trust before failing to return. Recheck on every outcome;
// retain the original command error while Runtime independently refreshes UI state.
fn finish_mutation(
    result: Result<bool>,
    recheck: impl FnOnce() -> Result<CertificateStatus>,
    failure: ErrorCode,
) -> Result<CertificateStatus> {
    let current = recheck();
    if !result? {
        bail!(failure);
    }
    current
}
pub fn install(root: &Path) -> Result<CertificateStatus> {
    install_with_entry(root, &secret_entry("local-ca")?)
}
fn install_with_entry(root: &Path, entry: &keyring::Entry) -> Result<CertificateStatus> {
    Authority::create_with_entry(root, entry)?;
    let path = cert_path(root);
    #[cfg(target_os = "macos")]
    let result = quiet(
        Command::new("/usr/bin/security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-k"])
            .arg(PathBuf::from(std::env::var("HOME")?).join("Library/Keychains/login.keychain-db"))
            .arg(&path),
        MUTATION_TIMEOUT,
    );
    #[cfg(windows)]
    let result = quiet(
        Command::new("certutil.exe")
            .args(["-user", "-addstore", "Root"])
            .arg(&path),
        MUTATION_TIMEOUT,
    );
    #[cfg(not(any(target_os = "macos", windows)))]
    let result = Ok(false);
    let current = finish_mutation(
        result,
        || checked_status(root),
        ErrorCode::CertificateInstallFailed,
    )?;
    if !current.trusted {
        bail!(ErrorCode::CertificateInstallFailed);
    }
    Ok(current)
}
pub fn remove(root: &Path) -> Result<CertificateStatus> {
    remove_with_entry(root, &secret_entry("local-ca")?)
}
fn remove_with_entry(root: &Path, entry: &keyring::Entry) -> Result<CertificateStatus> {
    if !cert_path(root).exists() {
        return Ok(status(root));
    }
    #[cfg(target_os = "macos")]
    {
        // Remove only the trust entry of our exact certificate, not certificates by name.
        let before = checked_status(root)?;
        if before.trusted {
            let result = quiet(
                Command::new("/usr/bin/security")
                    .arg("remove-trusted-cert")
                    .arg(cert_path(root)),
                MUTATION_TIMEOUT,
            );
            finish_mutation(
                result,
                || checked_status(root),
                ErrorCode::CertificateRemoveFailed,
            )?;
        }
        if let Some(hash) = before.fingerprint {
            let keychain =
                PathBuf::from(std::env::var("HOME")?).join("Library/Keychains/login.keychain-db");
            let result = quiet(
                Command::new("/usr/bin/security")
                    .args(["delete-certificate", "-Z", &hash])
                    .arg(keychain),
                MUTATION_TIMEOUT,
            );
            // An already absent keychain entry is harmless. A timeout must retain
            // the local pair so removal can safely be retried.
            finish_mutation(
                result.map(|_| true),
                || checked_status(root),
                ErrorCode::CertificateRemoveFailed,
            )?;
        }
    }
    #[cfg(windows)]
    {
        let result = quiet(Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $c=New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($env:GBF_CA_PATH); $s=New-Object System.Security.Cryptography.X509Certificates.X509Store('Root','CurrentUser'); try { $s.Open('ReadWrite'); foreach($item in $s.Certificates.Find('FindByThumbprint',$c.Thumbprint,$false)){if([Convert]::ToBase64String($item.RawData) -eq [Convert]::ToBase64String($c.RawData)){$s.Remove($item)}} } finally {$s.Close()}"]).env("GBF_CA_PATH", cert_path(root)), MUTATION_TIMEOUT);
        finish_mutation(
            result,
            || checked_status(root),
            ErrorCode::CertificateRemoveFailed,
        )?;
    }
    if checked_status(root)?.trusted {
        bail!(ErrorCode::CertificateRemoveFailed);
    }
    remove_pair(entry, || {
        fs::remove_file(cert_path(root))?;
        Ok(())
    })?;
    Ok(status(root))
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod native_tests {
    use super::*;

    #[test]
    #[ignore = "opt-in: creates and removes an isolated user test CA and credential"]
    fn native_certificate_and_credential_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let service = format!(
            "cc.lzsony.gbf-power-reborn.test.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let entry = keyring::Entry::new(&service, "local-ca").unwrap();
        struct Cleanup {
            dir: Option<tempfile::TempDir>,
            entry: keyring::Entry,
            service: String,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let dir = self.dir.take().unwrap();
                if remove_with_entry(dir.path(), &self.entry).is_err() {
                    // A timed-out trust mutation may have taken effect. Retain
                    // the exact public CA and secret identity for safe retry.
                    eprintln!(
                        "Test CA cleanup pending: directory={}, service={}",
                        dir.keep().display(),
                        self.service
                    );
                } else {
                    let _ = self.entry.delete_credential();
                }
            }
        }
        let cleanup = Cleanup {
            dir: Some(dir),
            entry,
            service,
        };
        let dir = cleanup.dir.as_ref().unwrap();
        let entry = &cleanup.entry;
        entry.set_password("isolated credential probe").unwrap();
        assert_eq!(entry.get_password().unwrap(), "isolated credential probe");
        entry.delete_credential().unwrap();
        Authority::create_with_entry(dir.path(), entry).unwrap();
        println!(
            "Isolated test CA SHA-256: {}",
            status(dir.path()).fingerprint.unwrap()
        );
        assert!(!status(dir.path()).trusted);
        Authority::load_with_entry(dir.path(), entry).unwrap();
        let before = fs::read(cert_path(dir.path())).unwrap();
        let installed = install_with_entry(dir.path(), entry).unwrap();
        assert!(installed.exists && installed.trusted);
        assert_eq!(fs::read(cert_path(dir.path())).unwrap(), before);
        assert!(install_with_entry(dir.path(), entry).unwrap().trusted);
        let removed = remove_with_entry(dir.path(), entry).unwrap();
        assert!(!removed.exists && !removed.trusted);
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    use std::cell::Cell;

    fn fake_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "certificate::command_tests::fake_certificate_command",
                "--ignored",
                "--nocapture",
            ])
            .env("GBF_TEST_CERTIFICATE_COMMAND", mode);
        command
    }

    #[test]
    #[ignore = "controlled subprocess fixture, launched only by command tests"]
    fn fake_certificate_command() {
        match std::env::var("GBF_TEST_CERTIFICATE_COMMAND").as_deref() {
            Ok("success") => (),
            Ok("cancel") => std::process::exit(5),
            Ok("hang") => std::thread::sleep(Duration::from_secs(60)),
            _ => panic!("fixture requires an explicit mode"),
        }
    }

    #[test]
    fn command_success_and_user_cancellation_are_distinct() {
        assert!(quiet(&mut fake_command("success"), Duration::from_secs(10)).unwrap());
        assert!(!quiet(&mut fake_command("cancel"), Duration::from_secs(10)).unwrap());
    }

    #[test]
    fn stalled_command_times_out_with_typed_error() {
        let start = Instant::now();
        let error = quiet(&mut fake_command("hang"), Duration::from_millis(100)).unwrap_err();
        assert_eq!(
            crate::error::CommandError::from_error(error, ErrorCode::InternalFailure).code,
            ErrorCode::CertificateOperationTimeout
        );
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn termination_waits_until_owned_process_has_exited() {
        let mut child = CommandChild::spawn(&mut fake_command("hang")).unwrap();
        child.terminate_and_wait().unwrap();
        assert!(child.reaped);
        assert!(child.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn mutation_always_rechecks_even_after_timeout_or_cancellation() {
        for result in [
            Ok(true),
            Ok(false),
            Err(anyhow::anyhow!(ErrorCode::CertificateOperationTimeout)),
        ] {
            let expected = match &result {
                Ok(true) => None,
                Ok(false) => Some(ErrorCode::CertificateInstallFailed),
                Err(_) => Some(ErrorCode::CertificateOperationTimeout),
            };
            let checked = Cell::new(false);
            let outcome = finish_mutation(
                result,
                || {
                    checked.set(true);
                    Ok(CertificateStatus {
                        exists: true,
                        trusted: true,
                        fingerprint: None,
                    })
                },
                ErrorCode::CertificateInstallFailed,
            );
            assert!(checked.get());
            assert_eq!(
                outcome
                    .err()
                    .map(|error| crate::error::CommandError::from_error(
                        error,
                        ErrorCode::InternalFailure
                    )
                    .code),
                expected
            );
        }
    }

    #[test]
    fn failed_post_mutation_verification_never_reports_success() {
        let result = finish_mutation(
            Ok(true),
            || Err(anyhow::anyhow!(ErrorCode::CertificateOperationTimeout)),
            ErrorCode::CertificateRemoveFailed,
        );
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct Secret {
        value: RefCell<Option<String>>,
        fail: Cell<bool>,
    }
    impl CaSecret for Secret {
        fn read(&self) -> Result<Option<String>> {
            Ok(self.value.borrow().clone())
        }
        fn write(&self, value: &str) -> Result<()> {
            if self.fail.replace(false) {
                bail!(ErrorCode::SecretWriteFailed);
            }
            *self.value.borrow_mut() = Some(value.into());
            Ok(())
        }
        fn delete(&self) -> Result<()> {
            *self.value.borrow_mut() = None;
            Ok(())
        }
    }
    #[test]
    fn failed_certificate_file_operations_preserve_secret_and_report_failure() {
        let secret = Secret {
            value: RefCell::new(Some("original".into())),
            fail: Cell::new(false),
        };
        assert!(save_pair(&secret, "replacement", || anyhow::bail!("file denied")).is_err());
        assert_eq!(secret.read().unwrap().as_deref(), Some("original"));
        assert!(remove_pair(&secret, || anyhow::bail!("file denied")).is_err());
        assert_eq!(secret.read().unwrap().as_deref(), Some("original"));
        secret.fail.set(true);
        assert!(save_pair(&secret, "replacement", || panic!(
            "must not write certificate after secret failure"
        ))
        .is_err());
        secret.delete().unwrap();
        assert!(save_pair(&secret, "new", || anyhow::bail!("file denied")).is_err());
        assert!(secret.read().unwrap().is_none());
    }
}
