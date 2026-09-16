use crate::{
    cache::{self, CacheKey, Cached},
    preferences::CachePreferences,
    proxy::ContextState,
    rules::GameLanguage,
};
use bytes::{Bytes, BytesMut};
use http::{header, HeaderMap, Request, Response};
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
    io::Read,
    num::NonZeroUsize,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const SOURCE_LIMIT: usize = 1024 * 1024;
#[derive(Clone)]
pub(crate) struct Discovery {
    pub uri: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}
#[derive(Default)]
pub(crate) struct Background {
    sender: Option<mpsc::Sender<Discovery>>,
    prefetch: Option<(CancellationToken, JoinHandle<()>)>,
    warmup: Option<(CancellationToken, JoinHandle<()>)>,
}
impl ContextState {
    pub(crate) fn discover(&self, uri: &str, headers: &HeaderMap, body: &Bytes) {
        let Ok(url) = url::Url::parse(uri) else {
            return;
        };
        if ![".js", ".json"].iter().any(|ext| url.path().ends_with(ext))
            || body.len() > SOURCE_LIMIT
        {
            return;
        }
        if let Ok(background) = self.background.try_lock() {
            if let Some(sender) = &background.sender {
                let _ = sender.try_send(Discovery {
                    uri: uri.into(),
                    headers: headers.clone(),
                    body: body.clone(),
                });
            }
        }
    }
    pub(crate) async fn configure_background(self: &Arc<Self>, prefs: CachePreferences) {
        let mut background = self.background.lock().await;
        let enabled =
            prefs.prefetch_enabled && self.authority.is_some() && !self.cancel.is_cancelled();
        if !enabled {
            background.sender = None;
            if let Some((cancel, handle)) = background.prefetch.take() {
                cancel.cancel();
                let _ = handle.await;
            }
        } else if background.prefetch.is_none() {
            let (tx, rx) = mpsc::channel(32);
            let cancel = self.cancel.child_token();
            let state = self.clone();
            let token = cancel.clone();
            let handle = tokio::spawn(async move {
                prefetch(state, rx, token).await;
            });
            background.sender = Some(tx);
            background.prefetch = Some((cancel, handle));
        }
        if !prefs.warmup_enabled || self.cancel.is_cancelled() {
            if let Some((cancel, handle)) = background.warmup.take() {
                cancel.cancel();
                let _ = handle.await;
            }
        } else if background.warmup.is_none() {
            let token = self.cancel.child_token();
            background.warmup = Some((
                token.clone(),
                tokio::spawn(self.cache.clone().warm(self.metrics.clone(), token)),
            ));
        }
    }
}

pub(crate) fn decode(data: &[u8], encoding: &str) -> Option<Vec<u8>> {
    let mut reader: Box<dyn Read + '_> = match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => Box::new(data),
        "gzip" => Box::new(flate2::read::GzDecoder::new(data)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(data)),
        "br" => Box::new(brotli::Decompressor::new(data, 4096)),
        _ => return None,
    };
    let mut out = Vec::new();
    reader
        .by_ref()
        .take((SOURCE_LIMIT + 1) as u64)
        .read_to_end(&mut out)
        .ok()?;
    (out.len() <= SOURCE_LIMIT).then_some(out)
}
fn quoted_strings(text: &str) -> Vec<String> {
    // Literal references only: never execute or evaluate JavaScript expressions.
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(quote) = chars.next() {
        if !matches!(quote, '\'' | '"' | '`') {
            continue;
        }
        let mut value = String::new();
        let mut valid = true;
        for c in chars.by_ref() {
            if c == quote {
                break;
            }
            value.push(c);
            if value.len() > 8192 {
                valid = false;
            }
        }
        if valid {
            out.push(value.replace("\\/", "/"));
        }
    }
    out
}
pub(crate) fn references(source: Discovery) -> Vec<String> {
    let Ok(base) = url::Url::parse(&source.uri) else {
        return vec![];
    };
    let Some(language) = GameLanguage::from_path(base.path()) else {
        return vec![];
    };
    let Some(decoded) = decode(
        &source.body,
        source
            .headers
            .get(header::CONTENT_ENCODING)
            .and_then(|h| h.to_str().ok())
            .unwrap_or(""),
    ) else {
        return vec![];
    };
    let Ok(text) = std::str::from_utf8(&decoded) else {
        return vec![];
    };
    let mut seen = HashSet::new();
    quoted_strings(text)
        .into_iter()
        .filter_map(|reference| {
            let mut url = base.join(&reference).ok()?;
            if !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || url.port_or_known_default() != Some(443)
                || url.scheme() != "https"
            {
                return None;
            }
            // Url normalizes the host; path and query remain part of cache identity.
            url.set_fragment(None);
            if GameLanguage::from_path(url.path()) != Some(language)
                || CacheKey::from_url(url.as_str()).is_none()
            {
                return None;
            }
            let value = url.to_string();
            seen.insert(value.clone()).then_some(value)
        })
        .take(120)
        .collect()
}
fn priority(uri: &str) -> u8 {
    let path = url::Url::parse(uri).unwrap().path().to_string();
    if [".js", ".css", ".json"].iter().any(|x| path.ends_with(x)) {
        0
    } else if [".mp3", ".ogg", ".m4a"].iter().any(|x| path.ends_with(x)) {
        2
    } else {
        1
    }
}
#[derive(Default)]
struct Queue {
    heap: BinaryHeap<Reverse<(u8, u64, String)>>,
    pending: HashSet<String>,
    sequence: u64,
}
impl Queue {
    fn push(&mut self, uri: String) {
        if self.pending.len() >= 256 || !self.pending.insert(uri.clone()) {
            return;
        }
        self.sequence += 1;
        self.heap
            .push(Reverse((priority(&uri), self.sequence, uri)));
    }
}
async fn prefetch(
    state: Arc<ContextState>,
    mut rx: mpsc::Receiver<Discovery>,
    cancel: CancellationToken,
) {
    let queue = Arc::new(Mutex::new(Queue::default()));
    let discovery = async {
        let mut seen = lru::LruCache::new(NonZeroUsize::new(4096).unwrap());
        loop {
            let source = tokio::select! { _ = cancel.cancelled() => break, item = rx.recv() => match item { Some(v) => v, None => break } };
            let result = tokio::task::spawn_blocking(move || {
                let identity = (CacheKey::from_url(&source.uri), cache::digest(&source.body));
                (identity, source)
            })
            .await;
            let Ok((identity, source)) = result else {
                continue;
            };
            if seen.put(identity, ()).is_some() {
                continue;
            }
            let Ok(urls) = tokio::task::spawn_blocking(move || references(source)).await else {
                continue;
            };
            if cancel.is_cancelled() {
                break;
            }
            for url in urls {
                queue.lock().unwrap().push(url);
            }
        }
    };
    let downloads = async {
        loop {
            tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(50)) => () }
            if state.metrics.foreground_busy() {
                continue;
            }
            let item = queue.lock().unwrap().heap.pop();
            let Some(Reverse((priority, sequence, uri))) = item else {
                continue;
            };
            let outcome = fetch(&state, &uri, &cancel).await;
            let mut queue = queue.lock().unwrap();
            if matches!(outcome, Ok(FetchOutcome::Deferred)) {
                queue.heap.push(Reverse((priority, sequence, uri)));
            } else {
                queue.pending.remove(&uri);
            }
        }
    };
    tokio::join!(discovery, downloads);
}
#[derive(Debug, PartialEq, Eq)]
enum FetchOutcome {
    Completed,
    Deferred,
    Cancelled,
}
async fn fetch(
    state: &Arc<ContextState>,
    uri: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<FetchOutcome> {
    let key = CacheKey::from_url(uri).ok_or_else(|| anyhow::anyhow!("invalid asset"))?;
    let lock = state.key_lock(&key);
    let _guard = tokio::select! { _ = cancel.cancelled() => return Ok(FetchOutcome::Cancelled), guard = lock.lock_owned() => guard };
    if state.metrics.foreground_busy() {
        return Ok(FetchOutcome::Deferred);
    }
    let request = Request::builder()
        .uri(uri)
        .header(header::ACCEPT_ENCODING, "identity")
        .body(())?;
    let cache = state.cache.clone();
    let k = key.clone();
    let cached = tokio::task::spawn_blocking(move || cache.get(&k)).await??;
    if cached
        .as_ref()
        .is_some_and(|entry| cache::fresh(entry, &request).is_some())
    {
        return Ok(FetchOutcome::Completed);
    }
    if cancel.is_cancelled() {
        return Ok(FetchOutcome::Cancelled);
    }
    if state.metrics.foreground_busy() {
        return Ok(FetchOutcome::Deferred);
    }
    #[cfg(test)]
    state.prefetch_waiting.notify_one();
    let _slot = tokio::select! {
        _ = cancel.cancelled() => return Ok(FetchOutcome::Cancelled),
        slot = state.fetch_slots.acquire() => slot?,
    };
    // Foreground may have arrived while this job waited for a key or a slot.
    if state.metrics.foreground_busy() {
        return Ok(FetchOutcome::Deferred);
    }
    let download = async {
        let mut response = state
            .upstream
            .get(uri)
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await?;
        let mut policy_response = Response::builder().status(response.status()).body(())?;
        *policy_response.headers_mut() = response.headers().clone();
        let Some(policy) = cache::policy(&request, &policy_response) else {
            return Ok::<_, anyhow::Error>(None);
        };
        if response.content_length().unwrap_or(0) > cache::MAX_OBJECT as u64 {
            return Ok(None);
        }
        let mut bytes = BytesMut::new();
        while let Some(chunk) = response.chunk().await? {
            state
                .metrics
                .received
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if bytes.len() + chunk.len() > cache::MAX_OBJECT {
                return Ok(None);
            }
            bytes.extend_from_slice(&chunk);
        }
        let path = url::Url::parse(uri)?.path().to_string();
        let bytes = bytes.freeze();
        let validation_bytes = bytes.clone();
        let headers = policy_response.headers().clone();
        if !tokio::task::spawn_blocking(move || {
            cache::valid_content(&path, &headers, &validation_bytes)
        })
        .await?
        {
            return Ok(None);
        }
        Ok(Some(Cached {
            policy,
            body: bytes,
        }))
    };
    let result = tokio::select! { _ = cancel.cancelled() => return Ok(FetchOutcome::Cancelled), result = tokio::time::timeout(Duration::from_secs(5),download) => result?? };
    if let Some(entry) = result {
        if cancel.is_cancelled() {
            return Ok(FetchOutcome::Cancelled);
        }
        let cache = state.cache.clone();
        let limit = state.settings.cache_limit_gb as u64 * 1024 * 1024 * 1024;
        tokio::task::spawn_blocking(move || cache.put(&key, entry, limit)).await??;
        state.metrics.downloads.fetch_add(1, Ordering::Relaxed);
        state.metrics.prefetched.fetch_add(1, Ordering::Relaxed);
    }
    Ok(FetchOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        certificate::Authority,
        config::Settings,
        proxy::tests::{origin, test_proxy},
    };
    use std::io::Write;

    #[tokio::test]
    async fn cancelled_inflight_prefetch_does_not_save_and_controlled_scene_measures_tradeoff() {
        let ca = Authority::ephemeral();
        let (address, calls, origin) = origin(Some(
            ca.server("prd-game-a-granbluefantasy.akamaized.net")
                .unwrap(),
        ))
        .await;
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(ca.pem().as_bytes()).unwrap())
            .resolve("prd-game-a-granbluefantasy.akamaized.net", address)
            .build()
            .unwrap();
        let local = Authority::ephemeral();
        let pem = local.pem();
        let (state, port, _dir, listener) =
            test_proxy(Settings::default(), Some(local), upstream).await;
        let browser = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap())
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
            .build()
            .unwrap();
        let cold_start = std::time::Instant::now();
        browser
            .get("https://prd-game-a-granbluefantasy.akamaized.net/assets/benchmark/cold.js")
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let cold_ms = cold_start.elapsed().as_millis();
        let before = calls.load(Ordering::Relaxed);
        state
            .configure_background(CachePreferences {
                prefetch_enabled: true,
                warmup_enabled: false,
            })
            .await;
        state.discover(
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/benchmark/manifest.json",
            &HeaderMap::new(),
            &Bytes::from_static(br#"["warm.js","unused.js"]"#),
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while state.metrics.prefetched.load(Ordering::Relaxed) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let loaded = calls.load(Ordering::Relaxed);
        assert_eq!(loaded - before, 2);
        let warm_start = std::time::Instant::now();
        browser
            .get("https://prd-game-a-granbluefantasy.akamaized.net/assets/benchmark/warm.js")
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let warm_ms = warm_start.elapsed().as_millis();
        assert_eq!(calls.load(Ordering::Relaxed), loaded);
        println!("CONTROLLED_SCENE origin_delay=100ms cold_scene={cold_ms}ms preloaded_scene={warm_ms}ms scene_origin_calls=1/0 total_downloads=1/2 extra_unused_body_bytes=11; prefetch completed before scene entry");
        state.discover(
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/slow-prefetch/manifest.json",
            &HeaderMap::new(),
            &Bytes::from_static(br#"["cancel.js"]"#),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::Relaxed) == loaded {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        state
            .configure_background(CachePreferences {
                prefetch_enabled: false,
                warmup_enabled: false,
            })
            .await;
        let key = CacheKey::from_url(
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/slow-prefetch/cancel.js",
        )
        .unwrap();
        assert!(state.cache.get(&key).unwrap().is_none());
        assert_eq!(state.metrics.prefetched.load(Ordering::Relaxed), 2);
        state.cancel.cancel();
        listener.await.unwrap();
        state.tasks.close();
        state.tasks.wait().await;
        origin.abort();
    }
    fn source(body: impl Into<Bytes>) -> Discovery {
        Discovery {
            uri: "https://prd-game-a-granbluefantasy.akamaized.net/assets/v1/manifest.json".into(),
            headers: HeaderMap::new(),
            body: body.into(),
        }
    }
    #[tokio::test]
    async fn dispatch_race_defers_and_retries_without_losing_work() {
        let ca = Authority::ephemeral();
        let (address, calls, origin) = origin(Some(
            ca.server("prd-game-a-granbluefantasy.akamaized.net")
                .unwrap(),
        ))
        .await;
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(ca.pem().as_bytes()).unwrap())
            .resolve("prd-game-a-granbluefantasy.akamaized.net", address)
            .build()
            .unwrap();
        let (state, _, _dir, listener) =
            test_proxy(Settings::default(), Some(Authority::ephemeral()), upstream).await;
        let slots = state.fetch_slots.acquire_many(16).await.unwrap();
        state
            .configure_background(CachePreferences {
                prefetch_enabled: true,
                warmup_enabled: false,
            })
            .await;
        state.discover(
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/manifest.json",
            &HeaderMap::new(),
            &Bytes::from_static(br#"["race.js"]"#),
        );
        tokio::time::timeout(Duration::from_secs(2), state.prefetch_waiting.notified())
            .await
            .unwrap();
        let foreground = state.metrics.foreground();
        drop(slots);
        // Wait until the first attempt relinquishes its key: dispatch has deferred.
        let key =
            CacheKey::from_url("https://prd-game-a-granbluefantasy.akamaized.net/assets/race.js")
                .unwrap();
        let lock = state.key_lock(&key);
        let guard = tokio::time::timeout(Duration::from_secs(2), lock.lock())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        drop(guard);
        drop(foreground);
        tokio::time::timeout(Duration::from_secs(3), async {
            while state.metrics.prefetched.load(Ordering::Relaxed) != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        state.cancel.cancel();
        state
            .configure_background(CachePreferences {
                prefetch_enabled: false,
                warmup_enabled: false,
            })
            .await;
        listener.await.unwrap();
        state.tasks.close();
        state.tasks.wait().await;
        origin.abort();
    }
    #[test]
    fn references_preserve_host_language_versions_queries_and_bounds() {
        let input = br#"["a.js?v=1", "a.js?v=2", "a.js?v=1", "/assets_en/v1/a.js", "https://evil.example/assets/a.js", "https://user:pw@prd-game-a-granbluefantasy.akamaized.net/assets/a.js", "https://prd-game-a-granbluefantasy.akamaized.net:444/assets/a.js", "/rest/a.json", "/assets/v2/a.js", "https://prd-game-a-granbluefantasy-steam.akamaized.net/assets/v1/a.js"]"#;
        let refs = references(source(Bytes::copy_from_slice(input)));
        assert_eq!(refs.len(), 4);
        assert!(refs[0].ends_with("a.js?v=1"));
        assert!(refs[1].ends_with("a.js?v=2"));
        assert!(refs[2].contains("/v2/"));
        assert!(refs[3].contains("prd-game-a-granbluefantasy-steam.akamaized.net"));
        let many = (0..200)
            .map(|i| format!("\"{i}.js\""))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(references(source(many)).len(), 120);
        assert!(references(source(vec![b'x'; SOURCE_LIMIT + 1])).is_empty());
        let mut english = source(br#"["a.js", "/assets/v1/a.js"]"#.to_vec());
        english.uri = english.uri.replace("/assets/", "/assets_en/");
        let refs = references(english);
        assert_eq!(refs.len(), 1);
        assert!(refs[0].contains("/assets_en/"));
    }
    #[test]
    fn bounded_decoding_handles_encodings_and_compression_bombs() {
        let data = b"[\"a.js\"]";
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(data).unwrap();
        assert_eq!(decode(&gzip.finish().unwrap(), "gzip").unwrap(), data);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zlib.write_all(data).unwrap();
        assert_eq!(decode(&zlib.finish().unwrap(), "deflate").unwrap(), data);
        let mut br = Vec::new();
        {
            let mut writer = brotli::CompressorWriter::new(&mut br, 4096, 3, 22);
            writer.write_all(data).unwrap();
        }
        assert_eq!(decode(&br, "br").unwrap(), data);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&vec![b'x'; SOURCE_LIMIT + 1]).unwrap();
        assert!(decode(&gzip.finish().unwrap(), "gzip").is_none());
        assert!(decode(b"broken", "gzip").is_none());
        assert!(decode(data, "unknown").is_none());
    }
    #[test]
    fn queues_are_bounded_deduplicated_and_prioritized() {
        let mut queue = Queue::default();
        queue.push("https://prd-game-a-granbluefantasy.akamaized.net/assets/a.mp3".into());
        queue.push("https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js".into());
        queue.push("https://prd-game-a-granbluefantasy.akamaized.net/assets/a.js".into());
        assert_eq!(queue.pending.len(), 2);
        assert!(queue.heap.pop().unwrap().0 .2.ends_with(".js"));
        for i in 0..400 {
            queue.push(format!(
                "https://prd-game-a-granbluefantasy.akamaized.net/assets/{i}.js"
            ));
        }
        assert_eq!(queue.pending.len(), 256);
        let (tx, _rx) = mpsc::channel(32);
        for _ in 0..32 {
            tx.try_send(source("[]")).unwrap();
        }
        assert!(tx.try_send(source("[]")).is_err());
    }
    #[tokio::test]
    async fn prefetch_yields_coalesces_and_browser_hits_count_only_browser_requests() {
        let ca = Authority::ephemeral();
        let (address, calls, origin) = origin(Some(
            ca.server("prd-game-a-granbluefantasy.akamaized.net")
                .unwrap(),
        ))
        .await;
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(ca.pem().as_bytes()).unwrap())
            .resolve("prd-game-a-granbluefantasy.akamaized.net", address)
            .build()
            .unwrap();
        let local_ca = Authority::ephemeral();
        let pem = local_ca.pem();
        let (state, port, _dir, listener) =
            test_proxy(Settings::default(), Some(local_ca), upstream).await;
        state
            .configure_background(CachePreferences {
                prefetch_enabled: true,
                warmup_enabled: false,
            })
            .await;
        let foreground = state.metrics.foreground();
        state.discover(
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/manifest.json",
            &HeaderMap::new(),
            &Bytes::from_static(br#"["test.js"]"#),
        );
        tokio::time::sleep(Duration::from_millis(180)).await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        drop(foreground);
        state.metrics.tunnel_activity();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        tokio::time::timeout(Duration::from_secs(3), async {
            while state.metrics.prefetched.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(state.metrics.requests.load(Ordering::Relaxed), 0);
        assert_eq!(state.metrics.hits.load(Ordering::Relaxed), 0);
        let token = CancellationToken::new();
        let uri = "https://prd-game-a-granbluefantasy.akamaized.net/assets/test.js";
        let (a, b) = tokio::join!(fetch(&state, uri, &token), fetch(&state, uri, &token));
        a.unwrap();
        b.unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let browser = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(pem.as_bytes()).unwrap())
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
            .build()
            .unwrap();
        assert_eq!(
            browser.get(uri).send().await.unwrap().text().await.unwrap(),
            "asset-bytes"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(state.metrics.requests.load(Ordering::Relaxed), 1);
        assert_eq!(state.metrics.hits.load(Ordering::Relaxed), 1);
        assert_eq!(state.metrics.downloads.load(Ordering::Relaxed), 1);
        state
            .configure_background(CachePreferences {
                prefetch_enabled: false,
                warmup_enabled: false,
            })
            .await;
        state.discover(
            "https://prd-game-a-granbluefantasy.akamaized.net/assets/other.json",
            &HeaderMap::new(),
            &Bytes::from_static(br#"["other.js"]"#),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        state.cancel.cancel();
        listener.await.unwrap();
        state.tasks.close();
        state.tasks.wait().await;
        origin.abort();
    }
}
