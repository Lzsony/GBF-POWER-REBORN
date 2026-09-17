use crate::{
    config::{Mode, Settings},
    metrics::Metrics,
    rules,
};
use anyhow::{bail, Context, Result};
use base64::Engine;
use std::{
    io,
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
};

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type BoxStream = Box<dyn Stream>;

pub fn client(
    settings: &Settings,
    password: &str,
    route_upstream: bool,
) -> Result<reqwest::Client> {
    if route_upstream && settings.mode == Mode::Accelerate {
        bail!(crate::error::ErrorCode::SshConnectionFailed);
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(12))
        .read_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(20))
        .pool_max_idle_per_host(8);
    if route_upstream && settings.mode != Mode::Direct {
        let scheme = if settings.mode == Mode::Http {
            "http"
        } else {
            "socks5h"
        };
        let host = settings.upstream_host.trim_matches(['[', ']']);
        let authority = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.into()
        };
        let mut proxy =
            reqwest::Proxy::all(format!("{scheme}://{authority}:{}", settings.upstream_port))?;
        if !settings.username.is_empty() {
            proxy = proxy.basic_auth(&settings.username, password);
        }
        builder = builder.proxy(proxy);
    }
    Ok(builder.build()?)
}

pub async fn connect(
    host: &str,
    port: u16,
    settings: &Settings,
    password: &str,
) -> Result<BoxStream> {
    tokio::time::timeout(
        Duration::from_secs(12),
        connect_inner(host, port, settings, password),
    )
    .await
    .context("連線逾時")?
}

async fn connect_inner(
    host: &str,
    port: u16,
    settings: &Settings,
    password: &str,
) -> Result<BoxStream> {
    if !rules::is_target(host) || settings.mode == Mode::Direct {
        let s = TcpStream::connect((host, port))
            .await
            .context("無法連線至來源")?;
        s.set_nodelay(true)?;
        return Ok(Box::new(s));
    }
    if settings.mode == Mode::Accelerate {
        bail!(crate::error::ErrorCode::SshConnectionFailed);
    }
    // Resolve upstream first to catch hostname aliases pointing back to this listener.
    let addresses: Vec<_> = tokio::net::lookup_host((
        settings.upstream_host.trim_matches(['[', ']']),
        settings.upstream_port,
    ))
    .await?
    .collect();
    if addresses
        .iter()
        .any(|a| a.ip().is_loopback() && a.port() == settings.listen_port)
    {
        bail!("上游指向本機代理，已阻止迴圈");
    }
    if settings.mode == Mode::Socks5 {
        let proxy = (
            settings.upstream_host.trim_matches(['[', ']']),
            settings.upstream_port,
        );
        let stream = if settings.username.is_empty() {
            tokio_socks::tcp::Socks5Stream::connect(proxy, (host, port)).await?
        } else {
            tokio_socks::tcp::Socks5Stream::connect_with_password(
                proxy,
                (host, port),
                &settings.username,
                password,
            )
            .await?
        };
        return Ok(Box::new(stream));
    }
    let mut s = TcpStream::connect(addresses.as_slice()).await?;
    s.set_nodelay(true)?;
    let auth = if settings.username.is_empty() {
        String::new()
    } else {
        format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{password}", settings.username))
        )
    };
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    s.write_all(
        format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{auth}\r\n").as_bytes(),
    )
    .await?;
    let mut header = Vec::new();
    loop {
        if header.len() >= 16384 {
            bail!("上游 CONNECT 回應過大");
        }
        header.push(s.read_u8().await?);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let status = std::str::from_utf8(&header)?
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("");
    if status != "200" {
        bail!("上游拒絕 CONNECT（{status}）");
    }
    Ok(Box::new(s))
}

pub struct Metered<S> {
    pub inner: S,
    pub metrics: Arc<Metrics>,
}
impl<S: AsyncRead + Unpin> AsyncRead for Metered<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        self.metrics
            .received
            .fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
        if buf.filled().len() > before {
            self.metrics.tunnel_activity();
        }
        poll
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Metered<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, bytes);
        if let Poll::Ready(Ok(n)) = poll {
            self.metrics.sent.fetch_add(n as u64, Ordering::Relaxed);
            if n > 0 {
                self.metrics.tunnel_activity();
            }
        }
        poll
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn socks_auth_and_remote_domain_are_preserved() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mock = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            assert_eq!(s.read_u8().await.unwrap(), 5);
            let count = s.read_u8().await.unwrap();
            let mut methods = vec![0; count as usize];
            s.read_exact(&mut methods).await.unwrap();
            assert!(methods.contains(&2));
            s.write_all(&[5, 2]).await.unwrap();
            assert_eq!(s.read_u8().await.unwrap(), 1);
            let count = s.read_u8().await.unwrap();
            let mut user = vec![0; count as usize];
            s.read_exact(&mut user).await.unwrap();
            let count = s.read_u8().await.unwrap();
            let mut pass = vec![0; count as usize];
            s.read_exact(&mut pass).await.unwrap();
            assert_eq!(user, b"test-user");
            assert_eq!(pass, b"test-password");
            s.write_all(&[1, 0]).await.unwrap();
            let mut header = [0u8; 4];
            s.read_exact(&mut header).await.unwrap();
            assert_eq!(header, [5, 1, 0, 3]);
            let count = s.read_u8().await.unwrap();
            let mut host = vec![0; count as usize];
            s.read_exact(&mut host).await.unwrap();
            assert_eq!(host, b"game.granbluefantasy.jp");
            assert_eq!(s.read_u16().await.unwrap(), 443);
            s.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 1, 187])
                .await
                .unwrap();
            let mut body = [0; 4];
            s.read_exact(&mut body).await.unwrap();
            assert_eq!(&body, b"ping");
            s.write_all(b"pong").await.unwrap();
        });
        let settings = Settings {
            mode: Mode::Socks5,
            upstream_port: port,
            username: "test-user".into(),
            ..Default::default()
        };
        let mut stream = connect("game.granbluefantasy.jp", 443, &settings, "test-password")
            .await
            .unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut body = [0; 4];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"pong");
        mock.await.unwrap();
    }
    #[tokio::test]
    async fn failed_http_auth_does_not_fall_back() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mock = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(s.read_u8().await.unwrap());
            }
            let header = String::from_utf8(header).unwrap();
            assert!(header.contains("Proxy-Authorization: Basic "));
            s.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let settings = Settings {
            mode: Mode::Http,
            upstream_port: port,
            username: "test-user".into(),
            ..Default::default()
        };
        let error = connect("game.granbluefantasy.jp", 443, &settings, "test-password")
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("407"));
        assert!(!error.contains("test-password"));
        mock.await.unwrap();
    }
}
