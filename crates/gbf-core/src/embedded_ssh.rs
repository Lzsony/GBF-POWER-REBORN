//! In-process SSH transport for iOS. The SOCKS gate only accepts named GBF destinations.
pub use crate::acceleration_types::*;
use crate::{config::Settings, error::ErrorCode, rules};
use anyhow::{bail, Context, Result};
use russh::{client, keys::PrivateKeyWithHashAlg};
use std::{
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

struct PinnedClient {
    key: Vec<u8>,
}
impl client::Handler for PinnedClient {
    type Error = anyhow::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool> {
        if key.certificate().is_some() || key.public_key().to_bytes()? != self.key {
            bail!(ErrorCode::SshHostKeyMismatch);
        }
        Ok(true)
    }
}
type Session = Arc<client::Handle<PinnedClient>>;
type Gate = Arc<RwLock<Option<(Session, CancellationToken)>>>;

fn expected_key(root: &Path, line: &Line) -> Result<Vec<u8>> {
    let text = std::fs::read_to_string(line.file(root, &line.known_hosts_file)?)?;
    let fields: Vec<_> = text.split_whitespace().collect();
    let authority = if line.port == 22 {
        line.host.clone()
    } else {
        format!("[{}]:{}", line.host, line.port)
    };
    anyhow::ensure!(
        fields.len() == 3 && fields[0] == authority && fields[1] == "ssh-ed25519",
        ErrorCode::SshNotConfigured
    );
    Ok(ssh_key::PublicKey::from_openssh(&format!("{} {}", fields[1], fields[2]))?.to_bytes()?)
}

async fn connect(root: &Path, line: &Line) -> Result<Session> {
    let pinned = PinnedClient {
        key: expected_key(root, line)?,
    };
    let pem = crate::authorization::device_key(root, &line.identity_file)?
        .to_openssh(ssh_key::LineEnding::LF)?;
    let key = russh::keys::decode_secret_key(&pem, None)?;
    let config = client::Config {
        keepalive_interval: Some(Duration::from_secs(10)),
        keepalive_max: 3,
        window_size: 256 * 1024,
        maximum_packet_size: 32 * 1024,
        ..Default::default()
    };
    let mut handle =
        client::connect(Arc::new(config), (line.host.as_str(), line.port), pinned).await?;
    let auth = handle
        .authenticate_publickey(
            &line.username,
            PrivateKeyWithHashAlg::new(Arc::new(key), None),
        )
        .await?;
    if !auth.success() {
        bail!(ErrorCode::SshAuthenticationFailed);
    }
    Ok(Arc::new(handle))
}

async fn destination(socket: &mut TcpStream) -> Result<(String, u16)> {
    anyhow::ensure!(socket.read_u8().await? == 5, "SOCKS version");
    let count = socket.read_u8().await? as usize;
    anyhow::ensure!(count > 0, "SOCKS methods");
    let mut methods = vec![0; count];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        socket.write_all(&[5, 255]).await?;
        bail!("SOCKS authentication");
    }
    socket.write_all(&[5, 0]).await?;
    let mut header = [0; 4];
    socket.read_exact(&mut header).await?;
    // No UDP, BIND, or IP destinations: Gateway authorization is domain based.
    anyhow::ensure!(header == [5, 1, 0, 3], "SOCKS command");
    let length = socket.read_u8().await? as usize;
    anyhow::ensure!(length > 0, "SOCKS host");
    let mut host = vec![0; length];
    socket.read_exact(&mut host).await?;
    let host = rules::normalize(std::str::from_utf8(&host)?);
    let port = socket.read_u16().await?;
    anyhow::ensure!(
        rules::is_target(&host) && matches!(port, 80 | 443),
        "Destination denied"
    );
    Ok((host, port))
}

async fn forward(mut socket: TcpStream, session: Session) -> Result<()> {
    let (host, port) =
        tokio::time::timeout(Duration::from_secs(5), destination(&mut socket)).await??;
    let channel = tokio::time::timeout(
        Duration::from_secs(12),
        session.channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0),
    )
    .await??;
    socket.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    let mut stream = channel.into_stream();
    tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
    Ok(())
}

fn close_gate(gate: &Gate) {
    if let Some((_, token)) = gate.write().unwrap().take() {
        token.cancel();
    }
}

pub struct Tunnel {
    pub port: u16,
    pub line: Line,
    pub status: SharedStatus,
    gate: Gate,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.cancel.cancel();
        close_gate(&self.gate);
    }
}
impl Tunnel {
    pub async fn start(root: &Path, line: Line, status: SharedStatus) -> Result<Self> {
        Self::start_cancellable(root, line, status, CancellationToken::new()).await
    }
    pub async fn start_cancellable(
        root: &Path,
        line: Line,
        status: SharedStatus,
        external: CancellationToken,
    ) -> Result<Self> {
        line.validate()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let gate: Gate = Arc::new(RwLock::new(None));
        let cancel = CancellationToken::new();
        let (tx, rx) = oneshot::channel();
        let mut ready = Some(tx);
        let root = root.to_path_buf();
        let task_gate = gate.clone();
        let task_cancel = cancel.clone();
        let task_line = line.clone();
        let task_status = status.clone();
        let task = tokio::spawn(async move {
            let accept_cancel = task_cancel.clone();
            let accept_gate = task_gate.clone();
            let accept = tokio::spawn(async move {
                let mut streams = JoinSet::new();
                loop {
                    tokio::select! {
                        _ = accept_cancel.cancelled() => break,
                        Some(_) = streams.join_next(), if !streams.is_empty() => {},
                        result = listener.accept() => {
                            let Ok((socket, _)) = result else { break };
                            let current = accept_gate.read().unwrap().clone();
                            if let Some((session, token)) = current {
                                if streams.len() < 32 {
                                    streams.spawn(async move { tokio::select! { _ = token.cancelled() => {}, _ = forward(socket, session) => {} } });
                                }
                            }
                        }
                    }
                }
                streams.abort_all();
                while streams.join_next().await.is_some() {}
            });
            let mut failures = 0u32;
            loop {
                set_status(
                    &task_status,
                    &task_line,
                    if ready.is_some() {
                        Phase::Connecting
                    } else {
                        Phase::Reconnecting
                    },
                    None,
                );
                let outcome = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    value = tokio::time::timeout(Duration::from_secs(15), connect(&root, &task_line)) => value.context(ErrorCode::SshConnectionFailed).and_then(|v| v),
                };
                match outcome {
                    Ok(session) => {
                        failures = 0;
                        *task_gate.write().unwrap() =
                            Some((session.clone(), task_cancel.child_token()));
                        set_status(&task_status, &task_line, Phase::Connected, None);
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Ok(()));
                        }
                        loop {
                            if session.is_closed() {
                                break;
                            }
                            tokio::select! { _ = task_cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
                        }
                        close_gate(&task_gate);
                        let _ = tokio::time::timeout(
                            Duration::from_secs(1),
                            session.disconnect(russh::Disconnect::ByApplication, "stopped", "en"),
                        )
                        .await;
                        if task_cancel.is_cancelled() {
                            break;
                        }
                    }
                    Err(error) => {
                        let code = crate::error::CommandError::from_error(
                            error,
                            ErrorCode::SshConnectionFailed,
                        )
                        .code;
                        set_status(&task_status, &task_line, Phase::Error, Some(code));
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Err(code));
                            break;
                        }
                        if matches!(
                            code,
                            ErrorCode::SshHostKeyMismatch
                                | ErrorCode::SshAuthenticationFailed
                                | ErrorCode::SshNotConfigured
                        ) {
                            break;
                        }
                        failures = failures.saturating_add(1);
                    }
                }
                tokio::select! { _ = task_cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs((1u64 << failures.min(4)).min(15))) => {} }
            }
            close_gate(&task_gate);
            task_cancel.cancel();
            let _ = accept.await;
        });
        let mut tunnel = Self {
            port,
            line,
            status,
            gate,
            cancel,
            task: Some(task),
        };
        let result = tokio::select! { _ = external.cancelled() => Err(ErrorCode::ConnectionTestFailed), result = rx => result.unwrap_or(Err(ErrorCode::SshConnectionFailed)) };
        if let Err(code) = result {
            tunnel.stop().await;
            return Err(code.into());
        }
        Ok(tunnel)
    }
    pub fn routed_settings(&self, settings: &Settings) -> Settings {
        let mut next = settings.clone();
        next.mode = crate::config::Mode::Socks5;
        next.upstream_host = "127.0.0.1".into();
        next.upstream_port = self.port;
        next.username.clear();
        next
    }
    pub fn session(&self) -> Option<CancellationToken> {
        self.gate
            .read()
            .unwrap()
            .as_ref()
            .map(|(_, token)| token.clone())
    }
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    pub(crate) fn session_reader(
        &self,
    ) -> impl Fn() -> Option<CancellationToken> + Send + Sync + 'static {
        let gate = self.gate.clone();
        move || {
            gate.read()
                .unwrap()
                .as_ref()
                .map(|(_, token)| token.clone())
        }
    }
    pub async fn stop(&mut self) {
        self.cancel.cancel();
        close_gate(&self.gate);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::server::{self, Server as _};

    #[derive(Clone)]
    struct EchoGateway {
        authorized: Vec<u8>,
        destinations: Arc<std::sync::Mutex<Vec<(String, u32)>>>,
    }
    impl server::Server for EchoGateway {
        type Handler = Self;
        fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
            self.clone()
        }
    }
    impl server::Handler for EchoGateway {
        type Error = russh::Error;
        async fn auth_publickey(
            &mut self,
            user: &str,
            key: &russh::keys::PublicKey,
        ) -> std::result::Result<server::Auth, Self::Error> {
            Ok(
                if user == "reborn" && key.to_bytes().unwrap() == self.authorized {
                    server::Auth::Accept
                } else {
                    server::Auth::Reject {
                        proceed_with_methods: None,
                        partial_success: false,
                    }
                },
            )
        }
        async fn channel_open_direct_tcpip(
            &mut self,
            channel: russh::Channel<server::Msg>,
            host: &str,
            port: u32,
            _: &str,
            _: u32,
            reply: server::ChannelOpenHandle,
            _: &mut server::Session,
        ) -> std::result::Result<(), Self::Error> {
            self.destinations.lock().unwrap().push((host.into(), port));
            reply.accept().await;
            tokio::spawn(async move {
                let stream = channel.into_stream();
                let (mut input, mut output) = tokio::io::split(stream);
                let _ = tokio::io::copy(&mut input, &mut output).await;
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn real_embedded_transport_authenticates_preserves_domains_and_closes_streams() {
        let root = tempfile::tempdir().unwrap();
        let device =
            ssh_key::PrivateKey::random(&mut rand_core::OsRng, ssh_key::Algorithm::Ed25519)
                .unwrap();
        device
            .write_openssh_file(&root.path().join("device"), ssh_key::LineEnding::LF)
            .unwrap();
        let host = ssh_key::PrivateKey::random(&mut rand_core::OsRng, ssh_key::Algorithm::Ed25519)
            .unwrap();
        let host = russh::keys::decode_secret_key(
            &host.to_openssh(ssh_key::LineEnding::LF).unwrap(),
            None,
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let public = host.public_key().to_openssh().unwrap();
        std::fs::write(
            root.path().join("known_hosts"),
            format!("[127.0.0.1]:{port} {public}\n"),
        )
        .unwrap();
        let line = Line {
            id: "test".into(),
            name: "test".into(),
            host: "127.0.0.1".into(),
            port,
            username: "reborn".into(),
            identity_file: "device".into(),
            known_hosts_file: "known_hosts".into(),
        };
        let destinations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut gateway = EchoGateway {
            authorized: device.public_key().to_bytes().unwrap(),
            destinations: destinations.clone(),
        };
        let config = Arc::new(server::Config {
            keys: vec![host],
            auth_rejection_time: Duration::ZERO,
            ..Default::default()
        });
        let (ready_tx, ready_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let server = gateway.run_on_socket(config, &listener);
            let _ = ready_tx.send(server.handle());
            server.await.unwrap();
        });
        let server_handle = ready_rx.await.unwrap();
        let mut tunnel = Tunnel::start(
            root.path(),
            line.clone(),
            Arc::new(RwLock::new(Default::default())),
        )
        .await
        .unwrap();
        let mut streams = Vec::new();
        for _ in 0..3 {
            let mut stream = tokio_socks::tcp::Socks5Stream::connect(
                ("127.0.0.1", tunnel.port),
                ("game.granbluefantasy.jp", 443),
            )
            .await
            .unwrap();
            stream.write_all(b"opaque TLS bytes").await.unwrap();
            let mut reply = [0; 16];
            stream.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"opaque TLS bytes");
            streams.push(stream);
        }
        assert_eq!(
            destinations.lock().unwrap().as_slice(),
            &vec![("game.granbluefantasy.jp".into(), 443); 3]
        );
        let gate_port = tunnel.port;
        tunnel.stop().await;
        for mut stream in streams {
            assert!(!matches!(
                tokio::time::timeout(Duration::from_secs(2), stream.read_u8()).await,
                Ok(Ok(_))
            ));
        }
        assert!(TcpListener::bind(("127.0.0.1", gate_port)).await.is_ok());
        // A real failed handshake must also retain the stable host-key error.
        let wrong = ssh_key::PrivateKey::random(&mut rand_core::OsRng, ssh_key::Algorithm::Ed25519)
            .unwrap();
        std::fs::write(
            root.path().join("known_hosts"),
            format!(
                "[127.0.0.1]:{port} {}\n",
                wrong.public_key().to_openssh().unwrap()
            ),
        )
        .unwrap();
        let failure = Tunnel::start(root.path(), line, Arc::new(RwLock::new(Default::default())))
            .await
            .err()
            .unwrap();
        assert_eq!(
            failure.downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::SshHostKeyMismatch)
        );
        server_handle.shutdown("test complete".into());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn socks_gate_requires_whitelisted_names_and_tcp_web_ports() {
        for (host, port, accepted) in [
            ("game.granbluefantasy.jp", 443u16, true),
            ("evil.com", 443, false),
            ("game.granbluefantasy.jp", 22, false),
            ("127.0.0.1", 443, false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut socket = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let server =
                tokio::spawn(
                    async move { destination(&mut listener.accept().await.unwrap().0).await },
                );
            socket.write_all(&[5, 1, 0]).await.unwrap();
            let mut method = [0; 2];
            socket.read_exact(&mut method).await.unwrap();
            assert_eq!(method, [5, 0]);
            socket
                .write_all(&[5, 1, 0, 3, host.len() as u8])
                .await
                .unwrap();
            socket.write_all(host.as_bytes()).await.unwrap();
            socket.write_u16(port).await.unwrap();
            assert_eq!(server.await.unwrap().is_ok(), accepted);
        }
    }
    #[tokio::test]
    async fn mismatched_host_key_is_a_terminal_error() {
        let key = ssh_key::PrivateKey::random(&mut rand_core::OsRng, ssh_key::Algorithm::Ed25519)
            .unwrap();
        let key =
            russh::keys::decode_secret_key(&key.to_openssh(ssh_key::LineEnding::LF).unwrap(), None)
                .unwrap();
        let mut handler = PinnedClient { key: vec![0] };
        use client::Handler;
        assert_eq!(
            handler
                .check_server_key(&key.public_key().clone().into())
                .await
                .unwrap_err()
                .downcast_ref::<ErrorCode>(),
            Some(&ErrorCode::SshHostKeyMismatch)
        );
    }
}
