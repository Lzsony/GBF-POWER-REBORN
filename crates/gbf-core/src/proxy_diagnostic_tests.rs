use super::*;
use std::{io, sync::OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn capture() -> Arc<Mutex<Vec<u8>>> {
    static CAPTURE: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    CAPTURE
        .get_or_init(|| {
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let writer = Capture(bytes.clone());
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::set_global_default(subscriber).unwrap();
            bytes
        })
        .clone()
}
fn log_for(id: &str) -> String {
    String::from_utf8(capture().lock().unwrap().clone())
        .unwrap()
        .lines()
        .filter(|line| line.contains(id))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn fault_origin(
    wire: &'static [u8],
    delay: Duration,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::atomic::AtomicU64>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let calls = count.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            calls.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let mut bytes = [0u8; 4096];
                let _ = stream.read(&mut bytes).await;
                tokio::time::sleep(delay).await;
                let _ = stream.write_all(wire).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (addr, count, task)
}
async fn run_fault(
    wire: &'static [u8],
    delay: Duration,
    timeout: Duration,
    path: &str,
) -> (u16, Option<String>, Result<Bytes, reqwest::Error>, u64) {
    capture();
    let (addr, count, source) = fault_origin(wire, delay).await;
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .read_timeout(timeout)
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let (state, port, _dir, listener) =
        tests::test_proxy(Settings::default(), None, upstream).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let response = client
        .get(format!(
            "http://prd-game-a-granbluefantasy.akamaized.net/{path}"
        ))
        .header("Origin", "https://game.granbluefantasy.jp")
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.bytes().await;
    let calls = count.load(Ordering::Relaxed);
    state.cancel.cancel();
    listener.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
    source.abort();
    (status, id, bytes, calls)
}

#[tokio::test]
async fn header_disconnect_returns_correlated_502_after_bounded_retry() {
    let (status, id, _, calls) = run_fault(
        b"",
        Duration::ZERO,
        Duration::from_secs(2),
        "assets/fail.js",
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(calls, 2);
    let log = log_for(&id.unwrap());
    assert!(log.contains("phase=\"upstream_send\""), "{log}");
    assert!(log.contains("EARLY_EOF"), "{log}");
}
#[tokio::test]
async fn buffered_body_disconnect_returns_correlated_502() {
    let wire = b"HTTP/1.1 200 OK\r\nCache-Control: public, max-age=600\r\nContent-Length: 100\r\nConnection: close\r\n\r\nabc";
    let (status, id, _, calls) = run_fault(
        wire,
        Duration::ZERO,
        Duration::from_secs(2),
        "assets/fail.js",
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(calls, 2);
    let log = log_for(&id.unwrap());
    assert!(log.contains("response_body_buffered"), "{log}");
    assert!(log.contains("EARLY_EOF"), "{log}");
}
#[tokio::test]
async fn timeout_is_classified_without_retry() {
    let (status, id, _, calls) = run_fault(
        b"",
        Duration::from_millis(200),
        Duration::from_millis(30),
        "assets/timeout.js",
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(calls, 1);
    assert!(log_for(&id.unwrap()).contains("TIMEOUT"));
}
#[tokio::test]
async fn upstream_502_is_not_relabelled_as_local() {
    let wire = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 3\r\nConnection: close\r\n\r\nerr";
    let (status, id, bytes, calls) = run_fault(
        wire,
        Duration::ZERO,
        Duration::from_secs(2),
        "assets/upstream.js",
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(calls, 1);
    assert!(id.is_none());
    assert_eq!(bytes.unwrap(), "err");
}
#[tokio::test]
async fn success_preserves_cors() {
    capture();
    let wire = b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: https://game.granbluefantasy.jp\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc";
    let (addr, _, source) = fault_origin(wire, Duration::ZERO).await;
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let (state, port, _dir, listener) =
        tests::test_proxy(Settings::default(), None, upstream).await;
    let c = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let r = c
        .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/a.js")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers()["access-control-allow-origin"],
        "https://game.granbluefantasy.jp"
    );
    assert!(!r.headers().contains_key(REQUEST_ID_HEADER));
    assert_eq!(r.bytes().await.unwrap(), "abc");
    state.cancel.cancel();
    listener.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
    source.abort();
}
#[tokio::test]
async fn refused_connection_is_classified() {
    capture();
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    drop(socket);
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let (state, port, _dir, listener) =
        tests::test_proxy(Settings::default(), None, upstream).await;
    let c = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let r = c
        .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/a.js")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert!(
        log_for(r.headers()[REQUEST_ID_HEADER].to_str().unwrap()).contains("CONNECTION_REFUSED")
    );
    state.cancel.cancel();
    listener.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
}

#[tokio::test]
async fn streaming_body_failure_does_not_replace_sent_response() {
    // Enough data is flushed before the disconnect to commit the original 200.
    capture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let source = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 4096];
        assert!(stream.read(&mut buf).await.unwrap() > 0);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nCache-Control: no-store\r\nContent-Length: 100\r\n\r\nabc",
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    });
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let (state, port, _dir, listener) =
        tests::test_proxy(Settings::default(), None, upstream).await;
    let c = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let r = c
        .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/stream.js")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(!r.headers().contains_key(REQUEST_ID_HEADER));
    assert!(r.bytes().await.is_err());
    source.await.unwrap();
    assert!(String::from_utf8(capture().lock().unwrap().clone())
        .unwrap()
        .contains("response_body_stream"));
    state.cancel.cancel();
    listener.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
}

#[tokio::test]
async fn untrusted_upstream_tls_is_classified() {
    capture();
    let source_ca = Authority::ephemeral();
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let tls = source_ca
        .server("prd-game-a-granbluefantasy.akamaized.net")
        .unwrap();
    let source = tokio::spawn(async move {
        let (stream, _) = socket.accept().await.unwrap();
        let _ = tokio_rustls::TlsAcceptor::from(tls).accept(stream).await;
    });
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let ca = Authority::ephemeral();
    let pem = ca.pem();
    let (state, port, _dir, listener) =
        tests::test_proxy(Settings::default(), Some(ca), upstream).await;
    let c = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap())
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let r = c
        .get("https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let log = log_for(r.headers()[REQUEST_ID_HEADER].to_str().unwrap());
    assert!(log.contains("TLS_VERIFICATION"), "{log}");
    assert!(log.contains("upstream_send"), "{log}");
    state.cancel.cancel();
    listener.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
    source.await.unwrap();
}

#[tokio::test]
async fn invalid_revalidation_is_classified() {
    capture();
    let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let source = tokio::spawn(async move {
        for wire in [
            b"HTTP/1.1 200 OK\r\nCache-Control: public, max-age=0\r\nETag: \"one\"\r\nContent-Type: text/javascript\r\nContent-Length: 8\r\nConnection: close\r\n\r\nlet a=1;".as_slice(),
            b"HTTP/1.1 304 Not Modified\r\nETag: \"two\"\r\nConnection: close\r\n\r\n".as_slice(),
            b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".as_slice(),
        ] {
            let (mut stream, _) = socket.accept().await.unwrap();
            let mut buf = [0; 4096]; assert!(stream.read(&mut buf).await.unwrap() > 0);
            stream.write_all(wire).await.unwrap();
        }
    });
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let (state, port, _dir, listener) =
        tests::test_proxy(Settings::default(), None, upstream).await;
    let c = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let url = "http://prd-game-a-granbluefantasy.akamaized.net/assets/revalidate.js";
    let r = c.get(url).send().await.unwrap();
    assert_eq!(r.status(), 200);
    r.bytes().await.unwrap();
    let r = c.get(url).send().await.unwrap();
    assert_eq!(r.status(), 502);
    let log = log_for(r.headers()[REQUEST_ID_HEADER].to_str().unwrap());
    assert!(log.contains("CACHE_REVALIDATION"), "{log}");
    state.cancel.cancel();
    listener.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
    source.await.unwrap();
}

#[tokio::test]
async fn post_failure_is_not_replayed() {
    capture();
    let (addr, count, source) = fault_origin(b"", Duration::ZERO).await;
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let (state, port, _dir, listener) =
        tests::test_proxy(Settings::default(), None, upstream).await;
    let c = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let r = c
        .post("http://prd-game-a-granbluefantasy.akamaized.net/action")
        .body("operation")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(count.load(Ordering::Relaxed), 1);
    state.cancel.cancel();
    listener.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
    source.abort();
}
