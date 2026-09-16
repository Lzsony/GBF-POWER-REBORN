use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const GOOD: &[u8] = b"HTTP/1.1 200 OK\r\nCache-Control: public, max-age=600\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: https://game.granbluefantasy.jp\r\nVary: Origin, Accept-Encoding\r\nContent-Length: 8\r\nConnection: close\r\n\r\nlet a=1;";
const STALE: &[u8] = b"HTTP/1.1 200 OK\r\nCache-Control: public, max-age=0\r\nETag: \"old\"\r\nContent-Type: text/javascript\r\nAccess-Control-Allow-Origin: https://game.granbluefantasy.jp\r\nContent-Length: 8\r\nConnection: close\r\n\r\nlet a=1;";
const BAD304: &[u8] = b"HTTP/1.1 304 Not Modified\r\nETag: \"new\"\r\nConnection: close\r\n\r\n";
struct Rig {
    state: Arc<ContextState>,
    _dir: tempfile::TempDir,
    client: reqwest::Client,
    calls: Arc<Mutex<Vec<String>>>,
    source: tokio::task::JoinHandle<()>,
    proxy: tokio::task::JoinHandle<()>,
}
impl Rig {
    async fn new(replies: Vec<&'static [u8]>, second_delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let capture = calls.clone();
        let source = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 16384 {
                    match stream.read_u8().await {
                        Ok(b) => bytes.push(b),
                        Err(_) => break,
                    }
                }
                let index = {
                    let mut c = capture.lock().unwrap();
                    let n = c.len();
                    c.push(String::from_utf8_lossy(&bytes).to_string());
                    n
                };
                let reply = *replies.get(index).unwrap_or(&b"".as_slice());
                tokio::spawn(async move {
                    if index > 0 {
                        tokio::time::sleep(second_delay).await;
                    }
                    let _ = stream.write_all(reply).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
            .build()
            .unwrap();
        let (state, port, dir, proxy) =
            tests::test_proxy(Settings::default(), None, upstream).await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
            .build()
            .unwrap();
        Self {
            state,
            _dir: dir,
            client,
            calls,
            source,
            proxy,
        }
    }
    async fn get(&self) -> reqwest::Response {
        self.client
            .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/recovery.js")
            .header("Origin", "https://game.granbluefantasy.jp")
            .header("Accept-Encoding", "identity")
            .send()
            .await
            .unwrap()
    }
    async fn close(self) {
        self.state.cancel.cancel();
        self.proxy.await.unwrap();
        self.state.tasks.close();
        self.state.tasks.wait().await;
        self.source.abort();
    }
}
#[tokio::test]
async fn mismatched_304_refetches_once_with_original_headers() {
    let rig = Rig::new(vec![STALE, BAD304, GOOD], Duration::ZERO).await;
    assert_eq!(rig.get().await.bytes().await.unwrap(), "let a=1;");
    let r = rig.get().await;
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers()["access-control-allow-origin"],
        "https://game.granbluefantasy.jp"
    );
    assert_eq!(r.bytes().await.unwrap(), "let a=1;");
    {
        let c = rig.calls.lock().unwrap();
        assert_eq!(c.len(), 3);
        assert!(c[1].to_lowercase().contains("if-none-match: \"old\""));
        assert!(!c[2].to_lowercase().contains("if-none-match"));
        assert!(c[2].contains("https://game.granbluefantasy.jp"));
        assert!(c[2].to_lowercase().contains("accept-encoding: identity"));
    }
    assert_eq!(rig.get().await.status(), 200);
    assert_eq!(rig.calls.lock().unwrap().len(), 3);
    rig.close().await;
}
#[tokio::test]
async fn missing_cache_and_transport_failures_share_two_attempt_budget() {
    for replies in [
        vec![BAD304, GOOD],
        vec![b"".as_slice(), GOOD],
        vec![BAD304, BAD304],
        vec![b"".as_slice(), BAD304],
        vec![BAD304, b"".as_slice()],
    ] {
        let expected = if replies[1] == GOOD { 200 } else { 502 };
        let rig = Rig::new(replies, Duration::ZERO).await;
        let r = rig.get().await;
        assert_eq!(r.status(), expected);
        r.bytes().await.unwrap();
        assert_eq!(rig.calls.lock().unwrap().len(), 2);
        rig.close().await;
    }
}
#[tokio::test]
async fn encoding_conflict_refetches_but_omitted_headers_are_preserved() {
    let conflicting = b"HTTP/1.1 304 Not Modified\r\nETag: \"old\"\r\nContent-Encoding: gzip\r\nConnection: close\r\n\r\n".as_slice();
    let rig = Rig::new(vec![STALE, conflicting, GOOD], Duration::ZERO).await;
    rig.get().await.bytes().await.unwrap();
    assert_eq!(rig.get().await.status(), 200);
    assert_eq!(rig.calls.lock().unwrap().len(), 3);
    rig.close().await;
    let valid =
        b"HTTP/1.1 304 Not Modified\r\nETag: \"old\"\r\nConnection: close\r\n\r\n".as_slice();
    let rig = Rig::new(vec![STALE, valid], Duration::ZERO).await;
    rig.get().await.bytes().await.unwrap();
    let r = rig.get().await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/javascript");
    assert_eq!(
        r.headers()["access-control-allow-origin"],
        "https://game.granbluefantasy.jp"
    );
    assert_eq!(r.bytes().await.unwrap(), "let a=1;");
    assert_eq!(rig.calls.lock().unwrap().len(), 2);
    rig.close().await;
}
#[tokio::test]
async fn replacement_404_is_forwarded_and_old_body_not_reused() {
    let not_found =
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice();
    let rig = Rig::new(vec![STALE, BAD304, not_found], Duration::ZERO).await;
    rig.get().await.bytes().await.unwrap();
    assert_eq!(rig.get().await.status(), 404);
    assert!(rig
        .state
        .cache
        .get(
            &CacheKey::from_url(
                "http://prd-game-a-granbluefantasy.akamaized.net/assets/recovery.js"
            )
            .unwrap()
        )
        .unwrap()
        .is_none());
    assert_eq!(rig.calls.lock().unwrap().len(), 3);
    rig.close().await;
}
#[tokio::test]
async fn credential_range_condition_and_body_requests_are_not_retried() {
    for header in [
        "cookie",
        "authorization",
        "range",
        "if-none-match",
        "if-modified-since",
        "if-match",
        "if-unmodified-since",
    ] {
        let rig = Rig::new(vec![b"", GOOD], Duration::ZERO).await;
        let r = rig
            .client
            .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/recovery.js")
            .header(header, "x")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 502);
        assert_eq!(rig.calls.lock().unwrap().len(), 1);
        rig.close().await;
    }
    let rig = Rig::new(vec![b"", GOOD], Duration::ZERO).await;
    let r = rig
        .client
        .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/recovery.js")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(rig.calls.lock().unwrap().len(), 1);
    rig.close().await;
}
#[tokio::test]
async fn buffered_partial_bytes_are_discarded_before_retry() {
    let partial = b"HTTP/1.1 200 OK\r\nCache-Control: public, max-age=600\r\nContent-Length: 100\r\nConnection: close\r\n\r\nBAD".as_slice();
    let rig = Rig::new(vec![partial, GOOD], Duration::ZERO).await;
    assert_eq!(rig.get().await.bytes().await.unwrap(), "let a=1;");
    assert_eq!(rig.calls.lock().unwrap().len(), 2);
    rig.close().await;
}
#[tokio::test]
async fn retry_has_five_second_budget() {
    let rig = Rig::new(vec![b"", GOOD], Duration::from_secs(10)).await;
    let start = tokio::time::Instant::now();
    assert_eq!(rig.get().await.status(), 502);
    assert!(start.elapsed() < Duration::from_secs(7));
    assert_eq!(rig.calls.lock().unwrap().len(), 2);
    rig.close().await;
}
#[tokio::test]
async fn same_asset_followers_share_recovery_and_stop_cancels_it() {
    let rig = Rig::new(vec![b"", GOOD], Duration::from_millis(50)).await;
    let results = futures_util::future::join_all((0..6).map(|_| rig.get())).await;
    for r in results {
        assert_eq!(r.status(), 200);
    }
    assert_eq!(rig.calls.lock().unwrap().len(), 2);
    rig.close().await;
    let rig = Rig::new(vec![b"", GOOD], Duration::from_secs(10)).await;
    let request = rig
        .client
        .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/recovery.js")
        .send();
    let cancel = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        rig.state.cancel.cancel();
    };
    let start = tokio::time::Instant::now();
    let _ = tokio::join!(request, cancel);
    assert!(start.elapsed() < Duration::from_secs(2));
    rig.close().await;
}
#[test]
fn retry_filter_is_closed_to_other_failures() {
    for kind in [
        std::io::ErrorKind::TimedOut,
        std::io::ErrorKind::ConnectionRefused,
        std::io::ErrorKind::Other,
    ] {
        assert!(!recoverable_transport(
            &std::io::Error::from(kind).into(),
            "upstream_send"
        ));
    }
    assert!(recoverable_transport(
        &std::io::Error::from(std::io::ErrorKind::ConnectionReset).into(),
        "upstream_send"
    ));
    assert!(recoverable_transport(
        &h2::Error::from(h2::Reason::REFUSED_STREAM).into(),
        "upstream_send"
    ));
}

#[tokio::test]
async fn http2_reset_is_retried_once() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let source = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let count = count.clone();
            tokio::spawn(async move {
                let mut conn = h2::server::handshake(socket).await.unwrap();
                while let Some(Ok((_, mut reply))) = conn.accept().await {
                    if count.fetch_add(1, Ordering::Relaxed) == 0 {
                        reply.send_reset(h2::Reason::PROTOCOL_ERROR);
                    } else {
                        let response = http::Response::builder()
                            .status(200)
                            .header("cache-control", "public, max-age=600")
                            .header("content-type", "text/javascript")
                            .body(())
                            .unwrap();
                        let mut stream = reply.send_response(response, false).unwrap();
                        stream
                            .send_data(Bytes::from_static(b"let a=1;"), true)
                            .unwrap();
                    }
                }
            });
        }
    });
    let upstream = reqwest::Client::builder()
        .no_proxy()
        .http2_prior_knowledge()
        .retry(reqwest::retry::never())
        .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
        .build()
        .unwrap();
    let (state, port, _dir, proxy) = tests::test_proxy(Settings::default(), None, upstream).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .build()
        .unwrap();
    let r = client
        .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/h2.js")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap(), "let a=1;");
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    state.cancel.cancel();
    proxy.await.unwrap();
    state.tasks.close();
    state.tasks.wait().await;
    source.abort();
}

#[tokio::test]
async fn invalidation_failure_stops_without_refetch() {
    let rig = Rig::new(vec![STALE, BAD304, GOOD], Duration::ZERO).await;
    rig.get().await.bytes().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while rig.state.metrics.downloads.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let path = std::fs::read_dir(rig.state.cache.root().join("ja"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(rig.get().await.status(), 502);
    assert_eq!(rig.calls.lock().unwrap().len(), 2);
    rig.close().await;
}
