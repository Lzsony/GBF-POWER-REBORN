//! App-owned SSH transport. The stable loopback gate closes on every transport
//! loss; neither existing streams nor new requests can fall back to direct.
use crate::{config::Settings, error::ErrorCode};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    process::Command,
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

const PROBE_URL: &str = "https://game.granbluefantasy.jp/";

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
    fn validate(&self) -> Result<()> {
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

    fn file(&self, root: &Path, relative: &Path) -> Result<PathBuf> {
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

fn set_status(status: &SharedStatus, line: &Line, state: Phase, error: Option<ErrorCode>) {
    *status.write().unwrap() = AccelerationStatus {
        state,
        line_id: Some(line.id.clone()),
        error,
    };
}

#[derive(Clone)]
struct Destination {
    port: u16,
    cancel: CancellationToken,
}
type Gate = Arc<RwLock<Option<Destination>>>;

fn close_gate(gate: &Gate) {
    if let Some(destination) = gate.write().unwrap().take() {
        destination.cancel.cancel();
    }
}

async fn relay(listener: TcpListener, gate: Gate, cancel: CancellationToken) {
    let mut streams = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(_) = streams.join_next(), if !streams.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((mut incoming, _)) = accepted else { break };
                let destination = gate.read().unwrap().clone();
                let Some(destination) = destination else { continue };
                if streams.len() >= 256 { continue; }
                streams.spawn(async move {
                    tokio::select! {
                        _ = destination.cancel.cancelled() => {},
                        _ = async {
                            if let Ok(mut outgoing) = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, destination.port)).await {
                                let _ = incoming.set_nodelay(true);
                                let _ = outgoing.set_nodelay(true);
                                let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                            }
                        } => {},
                    }
                });
            }
        }
    }
    streams.abort_all();
    while streams.join_next().await.is_some() {}
}

pub struct Tunnel {
    pub port: u16,
    pub line: Line,
    pub status: SharedStatus,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
    gate: Gate,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Tunnel {
    pub fn session(&self) -> Option<CancellationToken> {
        self.gate.read().unwrap().as_ref().map(|d| d.cancel.clone())
    }
    pub(crate) fn session_reader(
        &self,
    ) -> impl Fn() -> Option<CancellationToken> + Send + Sync + 'static {
        let gate = self.gate.clone();
        move || gate.read().unwrap().as_ref().map(|d| d.cancel.clone())
    }
    pub async fn start_cancellable(
        root: &Path,
        line: Line,
        status: SharedStatus,
        cancel: CancellationToken,
    ) -> Result<Self> {
        Self::start_internal(
            root,
            line,
            status,
            ssh_executable(),
            PROBE_URL.to_string(),
            cancel,
        )
        .await
    }
    pub async fn start(root: &Path, line: Line, status: SharedStatus) -> Result<Self> {
        Self::start_with(root, line, status, ssh_executable(), PROBE_URL.to_string()).await
    }

    pub(crate) async fn start_with(
        root: &Path,
        line: Line,
        status: SharedStatus,
        executable: PathBuf,
        probe_url: String,
    ) -> Result<Self> {
        Self::start_internal(
            root,
            line,
            status,
            executable,
            probe_url,
            CancellationToken::new(),
        )
        .await
    }
    pub(crate) async fn start_internal(
        root: &Path,
        line: Line,
        status: SharedStatus,
        executable: PathBuf,
        probe_url: String,
        external_cancel: CancellationToken,
    ) -> Result<Self> {
        set_status(&status, &line, Phase::Connecting, None);
        let result = async {
            line.validate()?;
            let identity = line.file(root, &line.identity_file)?;
            let known_hosts = line.file(root, &line.known_hosts_file)?;
            #[cfg(windows)]
            crate::windows_permissions::verify_key(&identity)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if std::fs::metadata(&identity)?.permissions().mode() & 0o077 != 0 {
                    bail!(ErrorCode::SshNotConfigured);
                }
            }
            // OpenSSH does not expand any user/system configuration when -F is supplied.
            let config = identity
                .parent()
                .ok_or(ErrorCode::SshNotConfigured)?
                .join("config");
            if !config.is_file() {
                bail!(ErrorCode::SshNotConfigured);
            }
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let port = listener.local_addr()?.port();
            let cancel = CancellationToken::new();
            let gate: Gate = Arc::new(RwLock::new(None));
            let (ready_tx, ready_rx) = oneshot::channel();
            let supervisor = Supervisor {
                line: line.clone(),
                status: status.clone(),
                executable,
                identity,
                known_hosts,
                config,
                probe_url,
                gate: gate.clone(),
                cancel: cancel.clone(),
            };
            let task_cancel = cancel.clone();
            let tunnel_gate = gate.clone();
            let task = tokio::spawn(async move {
                let relay = tokio::spawn(relay(listener, gate.clone(), task_cancel.clone()));
                supervisor.run(ready_tx).await;
                close_gate(&gate);
                task_cancel.cancelled().await;
                let _ = relay.await;
            });
            let mut tunnel = Self {
                port,
                line: line.clone(),
                status: status.clone(),
                cancel,
                task: Some(task),
                gate: tunnel_gate,
            };
            let ready = tokio::select! {
                outcome = ready_rx => outcome,
                _ = external_cancel.cancelled() => {
                    tunnel.stop().await;
                    bail!(ErrorCode::ConnectionTestFailed);
                }
            };
            match ready {
                Ok(Ok(())) => Ok(tunnel),
                outcome => {
                    let code = outcome
                        .ok()
                        .and_then(Result::err)
                        .unwrap_or(ErrorCode::SshConnectionFailed);
                    tunnel.stop().await;
                    Err(code.into())
                }
            }
        }
        .await;
        if let Err(error) = &result {
            let code = error
                .downcast_ref::<ErrorCode>()
                .copied()
                .unwrap_or(ErrorCode::SshConnectionFailed);
            set_status(&status, &line, Phase::Error, Some(code));
        }
        result
    }

    pub fn routed_settings(&self, settings: &Settings) -> Settings {
        let mut routed = settings.clone();
        routed.mode = crate::config::Mode::Socks5;
        routed.upstream_host = "127.0.0.1".into();
        routed.upstream_port = self.port;
        routed.username.clear();
        routed
    }

    pub async fn stop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

fn ssh_executable() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32/OpenSSH/ssh.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/usr/bin/ssh")
    }
}

fn known_hosts_option(path: &Path) -> String {
    #[cfg(windows)]
    let path = {
        // OpenSSH parses this as an ssh_config value, not a Windows argv path.
        // Backslashes in Rust's canonical \\?\ path are consumed as escapes.
        let value = path.to_string_lossy().replace('\\', "/");
        if let Some(unc) = value.strip_prefix("//?/UNC/") {
            format!("//{unc}")
        } else {
            value.strip_prefix("//?/").unwrap_or(&value).to_owned()
        }
    };
    #[cfg(not(windows))]
    let path = path.to_string_lossy();
    format!("UserKnownHostsFile=\"{path}\"")
}

#[cfg(windows)]
use crate::windows_process::Child;
#[cfg(not(windows))]
use tokio::process::Child;

struct Process {
    child: Child,
    stderr: Option<JoinHandle<()>>,
    reason: Arc<AtomicU8>,
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(task) = &self.stderr {
            task.abort();
        }
    }
}
impl Process {
    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        if let Some(task) = self.stderr.take() {
            let _ = task.await;
        }
    }
    async fn exit_error(&mut self) -> ErrorCode {
        #[cfg(windows)]
        let _ = self.child.kill().await;
        if let Some(task) = self.stderr.take() {
            let _ = task.await;
        }
        match self.reason.load(Ordering::Relaxed) {
            1 => ErrorCode::SshAuthenticationFailed,
            2 => ErrorCode::SshHostKeyMismatch,
            _ => ErrorCode::SshConnectionFailed,
        }
    }
}

struct Supervisor {
    line: Line,
    status: SharedStatus,
    executable: PathBuf,
    identity: PathBuf,
    known_hosts: PathBuf,
    config: PathBuf,
    probe_url: String,
    gate: Gate,
    cancel: CancellationToken,
}

fn stderr_reason(text: &str) -> u8 {
    if text.contains("Host key verification failed")
        || text.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
    {
        2
    } else if text.contains("Permission denied")
        || text.contains("Load key")
        || text.contains("UNPROTECTED PRIVATE KEY")
    {
        1
    } else {
        0
    }
}

fn delay(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(4)).min(15))
}
fn terminal(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::SshMissing
            | ErrorCode::SshAuthenticationFailed
            | ErrorCode::SshHostKeyMismatch
            | ErrorCode::SshNotConfigured
    )
}

impl Supervisor {
    async fn spawn(&self) -> Result<(Process, u16)> {
        let reservation = TcpListener::bind("127.0.0.1:0").await?;
        let port = reservation.local_addr()?.port();
        let mut command = Command::new(&self.executable);
        #[cfg(all(test, windows))]
        if self.executable.extension().is_some_and(|ext| ext == "py") {
            command = Command::new(
                std::env::var_os("GBF_TEST_PYTHON").unwrap_or_else(|| "python".into()),
            );
            command.arg(&self.executable);
        }
        command
            .args(["-N", "-T", "-F"])
            .arg(&self.config)
            .arg("-D")
            .arg(format!("127.0.0.1:{port}"))
            .arg("-i")
            .arg(&self.identity)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "GlobalKnownHostsFile=none",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "HostKeyAlgorithms=ssh-ed25519",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ConnectionAttempts=1",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
                "-o",
                "ForwardAgent=no",
                "-o",
                "ForwardX11=no",
                "-o",
                "Compression=no",
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "LogLevel=ERROR",
            ])
            .arg("-o")
            .arg(known_hosts_option(&self.known_hosts))
            .arg("-p")
            .arg(self.line.port.to_string())
            .arg("-l")
            .arg(&self.line.username)
            .arg(&self.line.host)
            .env("LC_ALL", "C")
            .env_remove("SSH_AUTH_SOCK")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        drop(reservation);
        #[cfg(windows)]
        let spawned = Child::spawn(command.as_std());
        #[cfg(not(windows))]
        let spawned = command.spawn();
        let mut child = spawned.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ErrorCode::SshMissing
            } else {
                ErrorCode::SshConnectionFailed
            }
        })?;
        let mut stderr = child.stderr.take().unwrap();
        let reason = Arc::new(AtomicU8::new(0));
        let recorded = reason.clone();
        let stderr = tokio::spawn(async move {
            // Retain only a bounded rolling buffer for categorization; never log SSH output.
            let mut tail = Vec::new();
            let mut buf = [0u8; 1024];
            while let Ok(count) = stderr.read(&mut buf).await {
                if count == 0 {
                    break;
                }
                tail.extend_from_slice(&buf[..count]);
                let code = stderr_reason(&String::from_utf8_lossy(&tail));
                if code != 0 {
                    recorded.store(code, Ordering::Relaxed);
                }
                if tail.len() > 4096 {
                    tail.drain(..tail.len() - 2048);
                }
            }
        });
        Ok((
            Process {
                child,
                stderr: Some(stderr),
                reason,
            },
            port,
        ))
    }

    async fn ready(&self, process: &mut Process, port: u16) -> Result<()> {
        let settings = Settings {
            mode: crate::config::Mode::Socks5,
            upstream_port: port,
            ..Settings::default()
        };
        let client = crate::routing::client(&settings, "", true)?;
        let started = tokio::time::Instant::now();
        let deadline = started + Duration::from_secs(20);
        loop {
            if process.child.try_wait()?.is_some() {
                bail!(process.exit_error().await);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(ErrorCode::SshConnectionFailed);
            }
            let observation = crate::probe::head(
                &client,
                &self.probe_url,
                Duration::from_secs(3)
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
            if observation.http_status.is_some()
                && !matches!(observation.sample, crate::metrics::ProbeSample::Success(_))
            {
                observation.log("tunnel_validation");
                bail!(ErrorCode::ConnectionTestFailed);
            }
            if matches!(observation.sample, crate::metrics::ProbeSample::Success(_)) {
                if process.child.try_wait()?.is_some() {
                    bail!(process.exit_error().await);
                }
                tracing::info!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "tunnel_ready_including_validation"
                );
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn run(self, ready: oneshot::Sender<std::result::Result<(), ErrorCode>>) {
        let mut ready = Some(ready);
        let mut attempt = 0;
        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            #[cfg(not(test))]
            {
                let host = self.line.host.clone();
                let port = self.line.port;
                let cancel = self.cancel.clone();
                // Independent, bounded diagnostic; never gates SSH startup.
                tokio::spawn(async move {
                    let started = tokio::time::Instant::now();
                    let outcome = tokio::select! {
                        _ = cancel.cancelled() => return,
                        result = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect((host.as_str(), port))) => result
                    };
                    tracing::info!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        connected = matches!(outcome, Ok(Ok(_))),
                        "node_tcp_diagnostic"
                    );
                });
            }
            let outcome = match self.spawn().await {
                Ok((mut process, port)) => {
                    let connected = tokio::select! {
                        _ = self.cancel.cancelled() => { process.stop().await; break; },
                        result = self.ready(&mut process, port) => result,
                    };
                    match connected {
                        Ok(()) => {
                            *self.gate.write().unwrap() = Some(Destination {
                                port,
                                cancel: CancellationToken::new(),
                            });
                            set_status(&self.status, &self.line, Phase::Connected, None);
                            if let Some(ready) = ready.take() {
                                let _ = ready.send(Ok(()));
                            }
                            attempt = 0;
                            tokio::select! {
                                _ = self.cancel.cancelled() => {
                                    close_gate(&self.gate);
                                    process.stop().await;
                                    break;
                                },
                                _ = process.child.wait() => {},
                            }
                            close_gate(&self.gate);
                            process.exit_error().await
                        }
                        Err(error) => {
                            process.stop().await;
                            error
                                .downcast_ref::<ErrorCode>()
                                .copied()
                                .unwrap_or(ErrorCode::SshConnectionFailed)
                        }
                    }
                }
                Err(error) => error
                    .downcast_ref::<ErrorCode>()
                    .copied()
                    .unwrap_or(ErrorCode::SshConnectionFailed),
            };
            if let Some(ready) = ready.take() {
                let _ = ready.send(Err(outcome));
                set_status(&self.status, &self.line, Phase::Error, Some(outcome));
                return;
            }
            if terminal(outcome) {
                set_status(&self.status, &self.line, Phase::Error, Some(outcome));
                return;
            }
            set_status(&self.status, &self.line, Phase::Reconnecting, None);
            tokio::select! { _ = self.cancel.cancelled() => break, _ = tokio::time::sleep(delay(attempt)) => {} }
            attempt = attempt.saturating_add(1);
        }
        set_status(&self.status, &self.line, Phase::Disconnected, None);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    fn fixture_line() -> Line {
        Line {
            id: "gateway-test".into(),
            name: "Fixture Gateway".into(),
            host: "127.0.0.1".into(),
            port: 2222,
            username: "reborn".into(),
            identity_file: "ssh/device_ed25519".into(),
            known_hosts_file: "ssh/known_hosts".into(),
        }
    }
    pub(crate) fn fixture(mode: &str) -> (tempfile::TempDir, Line, PathBuf, SharedStatus) {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new()
            .prefix("GBF 測試 ")
            .tempdir()
            .unwrap();
        let line = fixture_line();
        let ssh = dir.path().join("ssh");
        std::fs::create_dir(&ssh).unwrap();
        #[cfg(windows)]
        crate::windows_permissions::protect_directory(&ssh).unwrap();
        std::fs::write(dir.path().join(&line.identity_file), "test-only").unwrap();
        #[cfg(windows)]
        crate::windows_permissions::protect_new_key(&dir.path().join(&line.identity_file)).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            dir.path().join(&line.identity_file),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::write(dir.path().join(&line.known_hosts_file), "test-only").unwrap();
        std::fs::write(ssh.join("config"), "").unwrap();
        std::fs::write(ssh.join("mode"), mode).unwrap();
        let executable = dir.path().join(if cfg!(windows) {
            "fake-ssh.py"
        } else {
            "fake-ssh"
        });
        std::fs::write(&executable, include_str!("../tests/fixtures/fake-ssh.py")).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        (
            dir,
            line,
            executable,
            Arc::new(RwLock::new(Default::default())),
        )
    }

    async fn wait_for(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(8), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    async fn kill_pid(root: &Path) {
        let pid = std::fs::read_to_string(root.join("ssh/pid")).unwrap();
        #[cfg(unix)]
        assert!(Command::new("/bin/kill")
            .args(["-KILL", &pid])
            .status()
            .await
            .unwrap()
            .success());
        #[cfg(windows)]
        assert!(Command::new("taskkill.exe")
            .args(["/F", "/PID", pid.trim()])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .unwrap()
            .success());
    }

    #[tokio::test]
    async fn process_reconnects_then_auth_failure_keeps_gate_closed_until_stop() {
        let (dir, line, executable, status) = fixture("normal");
        let mut tunnel = Tunnel::start_with(
            dir.path(),
            line,
            status.clone(),
            executable,
            "http://game.granbluefantasy.jp/".into(),
        )
        .await
        .unwrap();
        assert_eq!(status.read().unwrap().state, Phase::Connected);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ssh/destination")).unwrap(),
            "game.granbluefantasy.jp:80"
        );
        let settings = tunnel.routed_settings(&Settings::default());
        let client = crate::routing::client(&settings, "", true).unwrap();
        client
            .head("http://game.granbluefantasy.jp/user-request")
            .send()
            .await
            .unwrap();
        let first_pid = std::fs::read_to_string(dir.path().join("ssh/pid")).unwrap();
        kill_pid(dir.path()).await;
        wait_for(|| status.read().unwrap().state == Phase::Reconnecting).await;
        assert!(client
            .head("http://game.granbluefantasy.jp/must-not-replay")
            .send()
            .await
            .is_err());
        wait_for(|| {
            status.read().unwrap().state == Phase::Connected
                && std::fs::read_to_string(dir.path().join("ssh/pid")).unwrap() != first_pid
        })
        .await;
        let requests = std::fs::read_to_string(dir.path().join("ssh/requests")).unwrap();
        assert_eq!(requests.matches("user-request").count(), 1);
        assert!(!requests.contains("must-not-replay"));
        std::fs::write(dir.path().join("ssh/mode"), "auth").unwrap();
        kill_pid(dir.path()).await;
        wait_for(|| status.read().unwrap().state == Phase::Error).await;
        assert_eq!(
            status.read().unwrap().error,
            Some(ErrorCode::SshAuthenticationFailed)
        );
        let port = tunnel.port;
        assert!(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_err());
        let mut denied = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        assert_eq!(denied.read(&mut [0]).await.unwrap(), 0);
        let failed_pid = std::fs::read_to_string(dir.path().join("ssh/pid")).unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ssh/pid")).unwrap(),
            failed_pid
        );
        tunnel.stop().await;
        assert!(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn initial_host_mismatch_and_missing_client_are_categorized() {
        let (dir, line, executable, status) = fixture("host");
        let result = Tunnel::start_with(
            dir.path(),
            line.clone(),
            status.clone(),
            executable,
            "http://test.invalid/".into(),
        )
        .await;
        assert!(matches!(
            result.err().unwrap().downcast_ref::<ErrorCode>(),
            Some(ErrorCode::SshHostKeyMismatch)
        ));
        let result = Tunnel::start_with(
            dir.path(),
            line,
            status.clone(),
            dir.path().join("missing"),
            "http://test.invalid/".into(),
        )
        .await;
        assert!(matches!(
            result.err().unwrap().downcast_ref::<ErrorCode>(),
            Some(ErrorCode::SshMissing)
        ));
    }

    #[tokio::test]
    async fn cancelling_start_reaps_child() {
        let (dir, line, executable, status) = fixture("slow");
        let root = dir.path().to_path_buf();
        let task = tokio::spawn(async move {
            Tunnel::start_with(
                &root,
                line,
                status,
                executable,
                "http://test.invalid/".into(),
            )
            .await
        });
        wait_for(|| dir.path().join("ssh/pid").exists()).await;
        let pid = std::fs::read_to_string(dir.path().join("ssh/pid")).unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while process_alive(&pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    pub(crate) fn process_alive(pid: &str) -> bool {
        #[cfg(unix)]
        {
            std::process::Command::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        }
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::{
                Foundation::CloseHandle,
                System::Threading::{
                    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                },
            };
            let Ok(pid) = pid.trim().parse() else {
                return false;
            };
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0;
            let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == 259;
            CloseHandle(handle);
            alive
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    #[ignore = "opt-in: system OpenSSH and Python asyncssh==2.21.1 on PYTHONPATH"]
    async fn native_real_openssh_pins_host_and_authenticates_in_unicode_path() {
        use ssh_key::{rand_core::OsRng, Algorithm, LineEnding, PrivateKey};
        let (dir, mut line, _, status) = fixture("normal");
        let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
        key.write_openssh_file(&dir.path().join(&line.identity_file), LineEnding::LF)
            .unwrap();
        #[cfg(windows)]
        crate::windows_permissions::protect_new_key(&dir.path().join(&line.identity_file)).unwrap();
        std::fs::write(
            dir.path().join("authorized.pub"),
            key.public_key().to_openssh().unwrap(),
        )
        .unwrap();
        let mut command = Command::new(std::env::var_os("GBF_TEST_PYTHON").unwrap_or_else(|| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        }));
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut server = command
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loopback-ssh.py"))
            .arg(dir.path())
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        wait_for(|| dir.path().join("ready.json").exists()).await;
        let ready: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("ready.json")).unwrap()).unwrap();
        line.host = "127.0.0.1".into();
        line.port = ready["port"].as_u64().unwrap() as u16;
        let known_hosts = dir.path().join(&line.known_hosts_file);
        let pinned = std::fs::read(&known_hosts).unwrap();
        let mut tunnel = Tunnel::start_with(
            dir.path(),
            line.clone(),
            status.clone(),
            ssh_executable(),
            "http://game.granbluefantasy.jp/".into(),
        )
        .await
        .unwrap();
        let client =
            crate::routing::client(&tunnel.routed_settings(&Settings::default()), "", true)
                .unwrap();
        assert!(client
            .head("http://game.granbluefantasy.jp/check")
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        let port = tunnel.port;
        tunnel.stop().await;
        assert!(TcpListener::bind(("127.0.0.1", port)).await.is_ok());
        let other = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
        std::fs::write(
            &known_hosts,
            format!(
                "[127.0.0.1]:{} {}\n",
                line.port,
                other.public_key().to_openssh().unwrap()
            ),
        )
        .unwrap();
        let error = Tunnel::start_with(
            dir.path(),
            line.clone(),
            status.clone(),
            ssh_executable(),
            "http://game.granbluefantasy.jp/".into(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::SshHostKeyMismatch)
        );
        std::fs::write(&known_hosts, pinned).unwrap();
        other
            .write_openssh_file(&dir.path().join(&line.identity_file), LineEnding::LF)
            .unwrap();
        let error = Tunnel::start_with(
            dir.path(),
            line,
            status,
            ssh_executable(),
            "http://game.granbluefantasy.jp/".into(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::SshAuthenticationFailed)
        );
        server.kill().await.unwrap();
    }

    #[test]
    fn catalog_validation_and_error_classification() {
        let mut line = fixture_line();
        line.identity_file = "../private".into();
        assert!(line.validate().is_err());
        line.identity_file = "/private".into();
        assert!(line.validate().is_err());
        assert_eq!(stderr_reason("Permission denied (publickey)."), 1);
        assert_eq!(stderr_reason("Host key verification failed."), 2);
        assert_eq!(stderr_reason("Connection timed out"), 0);
        assert_eq!(
            (0..6).map(|i| delay(i).as_secs()).collect::<Vec<_>>(),
            vec![1, 2, 4, 8, 15, 15]
        );
    }
    #[tokio::test]
    async fn closed_gate_never_connects_and_cancels_existing_streams() {
        use tokio::io::AsyncWriteExt;
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let gate: Gate = Arc::new(RwLock::new(None));
        let cancel = CancellationToken::new();
        let task = tokio::spawn(relay(listener, gate.clone(), cancel.clone()));
        let mut denied = TcpStream::connect(address).await.unwrap();
        assert_eq!(denied.read(&mut [0]).await.unwrap(), 0);
        *gate.write().unwrap() = Some(Destination {
            port: destination.local_addr().unwrap().port(),
            cancel: CancellationToken::new(),
        });
        let mut browser = TcpStream::connect(address).await.unwrap();
        let (mut origin, _) = destination.accept().await.unwrap();
        browser.write_all(b"test").await.unwrap();
        let mut bytes = [0; 4];
        origin.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"test");
        close_gate(&gate);
        assert_eq!(browser.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(origin.read(&mut [0]).await.unwrap(), 0);
        cancel.cancel();
        task.await.unwrap();
        assert!(TcpStream::connect(address).await.is_err());
    }
}
