use crate::{
    cache::{self, Cache, CacheKey, Cached},
    certificate::Authority,
    config::Settings,
    metrics::{ConnectionGuard, Metrics},
    proxy_diagnostics::{Diagnostic, REQUEST_ID_HEADER},
    routing, rules,
};
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use futures_util::{StreamExt, TryStreamExt};
use http::{header, HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use http_cache_semantics::{AfterResponse, BeforeRequest};
use hyper::{
    body::{Body as _, Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{atomic::Ordering, Arc, Mutex, Weak},
    time::{Duration, SystemTime},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex as AsyncMutex, Semaphore},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;
#[cfg(test)]
#[path = "proxy_diagnostic_tests.rs"]
mod diagnostic_tests;
#[cfg(test)]
#[path = "proxy_recovery_tests.rs"]
mod recovery_tests;
struct ConnectionLease {
    _count: ConnectionGuard,
    _slot: tokio::sync::OwnedSemaphorePermit,
}
fn body(data: impl Into<Bytes>) -> Body {
    Full::new(data.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}
fn message(code: StatusCode, text: &'static str) -> Response<Body> {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body(text))
        .unwrap()
}

#[derive(Debug)]
struct InvalidNotModified(&'static str);
impl std::fmt::Display for InvalidNotModified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidNotModified {}

fn recoverable_transport(error: &anyhow::Error, phase: &str) -> bool {
    matches!(
        crate::proxy_diagnostics::classify(error.as_ref(), phase),
        "CONNECTION_RESET"
            | "EARLY_EOF"
            | "HTTP2_RESET"
            | "HTTP2_REFUSED_STREAM"
            | "HTTP2_CANCEL"
            | "HTTP2_GOAWAY"
    )
}

pub struct ContextState {
    pub settings: Settings,
    pub password: String,
    pub cache: Arc<Cache>,
    pub metrics: Arc<Metrics>,
    pub authority: Option<Authority>,
    pub cancel: CancellationToken,
    pub tasks: TaskTracker,
    pub direct: reqwest::Client,
    pub upstream: reqwest::Client,
    locks: Mutex<HashMap<CacheKey, Weak<AsyncMutex<()>>>>,
    certs: Mutex<HashMap<String, Arc<rustls::ServerConfig>>>,
    pub(crate) fetch_slots: Arc<Semaphore>,
    write_gate: Arc<AsyncMutex<()>>,
}
impl ContextState {
    pub fn new(
        settings: Settings,
        password: String,
        cache: Arc<Cache>,
        authority: Option<Authority>,
        metrics: Arc<Metrics>,
    ) -> Result<Self> {
        Ok(Self {
            direct: routing::client(&settings, "", false)?,
            upstream: routing::client(&settings, &password, true)?,
            settings,
            password,
            cache,
            metrics,
            authority,
            cancel: CancellationToken::new(),
            tasks: TaskTracker::new(),
            locks: Mutex::new(HashMap::new()),
            certs: Mutex::new(HashMap::new()),
            fetch_slots: Arc::new(Semaphore::new(16)),
            write_gate: Arc::new(AsyncMutex::new(())),
        })
    }
    pub(crate) fn key_lock(&self, key: &CacheKey) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        locks.retain(|_, value| value.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key.clone(), Arc::downgrade(&lock));
        lock
    }
    fn persist_later(self: &Arc<Self>, key: CacheKey, entry: Cached, count_download: bool) {
        if self.cancel.is_cancelled() {
            return;
        }
        let Some(ticket) = self.cache.stage(key, entry, count_download) else {
            return;
        };
        let state = self.clone();
        // Stage bounds task count and retained bytes. Accepted work drains on normal stop.
        self.tasks.spawn(async move {
            let _writer = state.write_gate.lock().await;
            let cache = state.cache.clone();
            let limit = state.settings.cache_limit_gb as u64 * 1024 * 1024 * 1024;
            match tokio::task::spawn_blocking(move || cache.persist(ticket, limit)).await {
                Ok(Ok(Some(true))) => {
                    state.metrics.downloads.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Ok(_)) => {}
                _ => tracing::warn!("cache_write_failed"),
            }
        });
    }
    fn tls_config(&self, host: &str) -> Result<Arc<rustls::ServerConfig>> {
        let mut certs = self.certs.lock().unwrap();
        if let Some(cert) = certs.get(host) {
            return Ok(cert.clone());
        }
        let config = self
            .authority
            .as_ref()
            .context("HTTPS 快取未啟用")?
            .server(host)?;
        certs.insert(host.into(), config.clone());
        Ok(config)
    }
}

pub async fn serve(listener: TcpListener, state: Arc<ContextState>) {
    let flush_state = state.clone();
    state.tasks.spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! { _ = flush_state.cancel.cancelled() => break, _ = interval.tick() => {} }
            let cache = flush_state.cache.clone();
            if !matches!(tokio::task::spawn_blocking(move || cache.flush_accesses()).await, Ok(Ok(()))) {
                tracing::warn!("cache_access_flush_failed");
            }
        }
    });
    let connections = Arc::new(Semaphore::new(256));
    loop {
        tokio::select! {
            _ = state.cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { tracing::warn!("listener_accept_failed"); continue; };
                let Ok(slot) = connections.clone().try_acquire_owned() else { continue; };
                let child = state.clone();
                state.tasks.spawn(async move {
                    let guard = Arc::new(ConnectionLease { _count: child.metrics.connection(), _slot: slot });
                    let service_state = child.clone();
                    let service = service_fn(move |req| handle(req, service_state.clone(), None, guard.clone()));
                    tokio::select! {
                        _ = child.cancel.cancelled() => (),
                        _ = hyper::server::conn::http1::Builder::new().timer(hyper_util::rt::TokioTimer::new())
                            .header_read_timeout(Duration::from_secs(20)).serve_connection(TokioIo::new(stream), service).with_upgrades() => ()
                    }
                });
            }
        }
    }
}

fn handle(
    req: Request<Incoming>,
    state: Arc<ContextState>,
    tls_host: Option<String>,
    guard: Arc<ConnectionLease>,
) -> futures_util::future::BoxFuture<'static, Result<Response<Body>, Infallible>> {
    Box::pin(async move {
        let mut diagnostic = Diagnostic::new();
        diagnostic.mode = match state.settings.mode {
            crate::config::Mode::Direct => "direct",
            crate::config::Mode::Http => "http",
            crate::config::Mode::Socks5 => "socks5",
        };
        match process(req, state, tls_host, guard, &mut diagnostic).await {
            Ok(response) => Ok(response),
            Err(error) => {
                diagnostic.failure(error.as_ref(), Some(502));
                let mut response = message(
                    StatusCode::BAD_GATEWAY,
                    "來源或上游連線失敗；請檢查連線設定。",
                );
                response.headers_mut().insert(
                    REQUEST_ID_HEADER,
                    diagnostic
                        .id
                        .parse()
                        .expect("generated diagnostic ID is a valid header"),
                );
                Ok(response)
            }
        }
    })
}
async fn process(
    mut req: Request<Incoming>,
    state: Arc<ContextState>,
    tls_host: Option<String>,
    guard: Arc<ConnectionLease>,
    diagnostic: &mut Diagnostic,
) -> Result<Response<Body>> {
    if tls_host.is_none() && req.method() == Method::GET && req.uri().path() == "/proxy.pac" {
        let authority = req
            .uri()
            .authority()
            .map(|a| a.as_str())
            .or_else(|| {
                req.headers()
                    .get(header::HOST)
                    .and_then(|v| v.to_str().ok())
            })
            .unwrap_or("");
        if authority == format!("127.0.0.1:{}", state.settings.listen_port)
            || authority == format!("localhost:{}", state.settings.listen_port)
        {
            return Ok(Response::builder()
                .header(header::CONTENT_TYPE, "application/x-ns-proxy-autoconfig")
                .header(header::CACHE_CONTROL, "no-store")
                .body(body(rules::pac(state.settings.listen_port)))?);
        }
    }
    if req.method() == Method::CONNECT {
        if tls_host.is_some() {
            return Ok(message(
                StatusCode::METHOD_NOT_ALLOWED,
                "不允許巢狀 CONNECT",
            ));
        }
        let authority = req.uri().authority().context("CONNECT 目的地無效")?;
        let host = rules::normalize(authority.host());
        let port = authority.port_u16().unwrap_or(443);
        if port != 443 || !valid_destination(&host) {
            return Ok(message(StatusCode::FORBIDDEN, "不允許的 CONNECT 目的地"));
        }
        if state.authority.is_some() && rules::is_asset(&host) {
            diagnostic.phase = "browser_tls_config";
            let tls = state.tls_config(&host)?;
            let upgraded = hyper::upgrade::on(&mut req);
            let child = state.clone();
            let mut connection_diagnostic = diagnostic.clone();
            connection_diagnostic.route = "browser";
            state.tasks.spawn(async move {
                let work = async {
                    connection_diagnostic.phase = "browser_upgrade";
                    let stream = TokioIo::new(upgraded.await?);
                    connection_diagnostic.phase = "browser_tls_handshake";
                    let stream = tokio::time::timeout(Duration::from_secs(15), tokio_rustls::TlsAcceptor::from(tls).accept(stream)).await??;
                    connection_diagnostic.phase = "browser_http_connection";
                    let service_state = child.clone();
                    let service = service_fn(move |req| handle(req, service_state.clone(), Some(host.clone()), guard.clone()));
                    hyper::server::conn::http1::Builder::new().timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(Duration::from_secs(20))
                        .serve_connection(TokioIo::new(stream), service).await?;
                    Ok::<(), anyhow::Error>(())
                };
                tokio::select! { _ = child.cancel.cancelled() => (), result = work => if let Err(error) = result { connection_diagnostic.failure(error.as_ref(), None); } }
            });
        } else {
            state.metrics.requests.fetch_add(1, Ordering::Relaxed);
            diagnostic.phase = "tunnel_connect";
            diagnostic.route =
                if !rules::is_target(&host) || state.settings.mode == crate::config::Mode::Direct {
                    "direct"
                } else {
                    "upstream"
                };
            let stream = routing::connect(&host, port, &state.settings, &state.password).await?;
            let upgraded = hyper::upgrade::on(&mut req);
            let child = state.clone();
            state.tasks.spawn(async move {
                let _guard = guard;
                let work = async {
                    let mut browser = TokioIo::new(upgraded.await?);
                    let mut upstream = routing::Metered {
                        inner: stream,
                        metrics: child.metrics.clone(),
                    };
                    tokio::io::copy_bidirectional(&mut browser, &mut upstream).await?;
                    Ok::<(), anyhow::Error>(())
                };
                tokio::select! { _ = child.cancel.cancelled() => (), _ = work => () }
            });
        }
        return Ok(Response::new(body(Bytes::new())));
    }
    state.metrics.requests.fetch_add(1, Ordering::Relaxed);
    let uri = if let Some(host) = tls_host.as_ref() {
        if let Some(authority) = req.uri().authority() {
            if rules::normalize(authority.host()) != *host
                || authority.port_u16().unwrap_or(443) != 443
            {
                return Ok(message(StatusCode::MISDIRECTED_REQUEST, "HTTPS 主機不符"));
            }
        }
        if let Some(host_header) = req.headers().get(header::HOST) {
            let raw = host_header.to_str().unwrap_or("");
            if raw != host && raw != format!("{host}:443") {
                return Ok(message(StatusCode::MISDIRECTED_REQUEST, "HTTPS 主機不符"));
            }
        }
        format!(
            "https://{host}{}",
            req.uri()
                .path_and_query()
                .map(|v| v.as_str())
                .unwrap_or("/")
        )
    } else {
        if req.uri().scheme_str() != Some("http") {
            return Ok(message(
                StatusCode::BAD_REQUEST,
                "請使用 HTTP Proxy 或 HTTPS CONNECT",
            ));
        }
        req.uri().to_string()
    };
    let url = url::Url::parse(&uri)?;
    let host = url.host_str().context("缺少目的地主機")?;
    if !valid_destination(host)
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port_or_known_default(), Some(80 | 443))
    {
        return Ok(message(StatusCode::FORBIDDEN, "不允許的目的地"));
    }
    let (parts, incoming) = req.into_parts();
    let bodyless = incoming.is_end_stream();
    let mut headers = parts.headers;
    strip_hop_headers(&mut headers);
    headers.remove(header::HOST);
    let mut policy_request = Request::builder()
        .method(parts.method.clone())
        .uri(&uri)
        .body(())?;
    *policy_request.headers_mut() = headers.clone();
    let key = CacheKey::from_url(&uri);
    let candidate = bodyless && key.is_some() && cache::eligible(&policy_request);
    let original_headers = headers.clone();
    let asset_path = url.path().to_string();
    diagnostic.phase = "cache_lookup";
    diagnostic.cache = if candidate { "miss" } else { "bypass" };
    if candidate {
        state.metrics.eligible.fetch_add(1, Ordering::Relaxed);
        if let Some(entry) = state.cache.get_ram(key.as_ref().expect("cache candidate")) {
            if let Some(response) = cache::fresh(&entry, &policy_request) {
                state.metrics.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Response::from_parts(response, body(entry.body)));
            }
        }
    }
    let _lock = if candidate {
        Some(
            state
                .key_lock(key.as_ref().expect("cache candidate"))
                .lock_owned()
                .await,
        )
    } else {
        None
    };
    let mut cached = None;
    if candidate {
        let c = state.cache.clone();
        let k = key.clone().expect("cache candidate");
        cached = tokio::task::spawn_blocking(move || c.get(&k))
            .await?
            .unwrap_or(None);
        if let Some(entry) = &cached {
            match entry
                .policy
                .before_request(&policy_request, SystemTime::now())
            {
                BeforeRequest::Fresh(response) => {
                    state.metrics.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(Response::from_parts(response, body(entry.body.clone())));
                }
                BeforeRequest::Stale { request, matches } => {
                    diagnostic.cache = "stale";
                    if matches {
                        headers = request.headers;
                    } else {
                        cached = None;
                    }
                }
            }
        }
    }
    let _fetch_slot = if candidate {
        Some(state.fetch_slots.clone().acquire_owned().await?)
    } else {
        None
    };
    diagnostic.route =
        if !rules::is_target(host) || state.settings.mode == crate::config::Mode::Direct {
            "direct"
        } else {
            "upstream"
        };
    diagnostic.phase = "upstream_send";
    let selected = if rules::is_target(host) {
        &state.upstream
    } else {
        &state.direct
    };
    let metrics = state.metrics.clone();
    let stream = incoming.into_data_stream().map_ok(move |bytes| {
        metrics
            .sent
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        bytes
    });
    let mut first_body = Some(reqwest::Body::wrap_stream(stream));
    let guards = Arc::new((_fetch_slot, _lock));
    let mut deadline = None;
    loop {
        let request_body = first_body.take().unwrap_or_else(|| {
            reqwest::Body::wrap_stream(futures_util::stream::empty::<
                std::result::Result<Bytes, std::io::Error>,
            >())
        });
        let attempt = async {
            diagnostic.phase = "upstream_send";
            let mut response = selected
                .request(parts.method.clone(), url.clone())
                .headers(headers.clone())
                .body(request_body)
                .send()
                .await?;
            let status = response.status();
            if status.is_server_error() {
                diagnostic.upstream_status(status.as_u16());
            }
            diagnostic.phase = "response_headers";
            let mut response_headers = response.headers().clone();
            strip_hop_headers(&mut response_headers);
            let mut policy_response = Response::builder().status(status).body(())?;
            *policy_response.headers_mut() = response_headers.clone();
            if status == StatusCode::NOT_MODIFIED && candidate && cached.is_none() {
                diagnostic.phase = "cache_revalidation";
                return Err(InvalidNotModified("missing_entry").into());
            }
            if let Some(entry) = cached.clone() {
                if status == StatusCode::NOT_MODIFIED {
                    diagnostic.phase = "cache_revalidation";
                    let stored_headers = cache::stored_response_headers(&entry.policy)?;
                    if let Some(encoding) = response_headers.get(header::CONTENT_ENCODING) {
                        let old = stored_headers
                            .get(header::CONTENT_ENCODING)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("identity");
                        if !encoding
                            .to_str()
                            .unwrap_or("")
                            .trim()
                            .eq_ignore_ascii_case(old.trim())
                        {
                            return Err(InvalidNotModified("encoding_mismatch").into());
                        }
                    }
                    if let AfterResponse::NotModified(policy, parts) = entry.policy.after_response(
                        &policy_request,
                        &policy_response,
                        SystemTime::now(),
                    ) {
                        let may_store = policy.is_storable()
                            && !response_headers.contains_key(header::SET_COOKIE);
                        let updated = Cached {
                            policy,
                            body: entry.body.clone(),
                        };
                        let k = key.clone().expect("cache candidate");
                        if may_store {
                            state.persist_later(k, updated, false);
                        } else {
                            let c = state.cache.clone();
                            let _ = tokio::task::spawn_blocking(move || c.remove(&k)).await;
                        }
                        let mut response = Response::from_parts(parts, body(entry.body));
                        for value in response_headers.get_all(header::SET_COOKIE) {
                            response
                                .headers_mut()
                                .append(header::SET_COOKIE, value.clone());
                        }
                        return Ok(response);
                    }
                    let reason =
                        if stored_headers.get(header::ETAG) != response_headers.get(header::ETAG) {
                            if response_headers.contains_key(header::ETAG) {
                                "etag_mismatch"
                            } else {
                                "etag_omitted"
                            }
                        } else if stored_headers.get(header::LAST_MODIFIED)
                            != response_headers.get(header::LAST_MODIFIED)
                        {
                            if response_headers.contains_key(header::LAST_MODIFIED) {
                                "last_modified_mismatch"
                            } else {
                                "last_modified_omitted"
                            }
                        } else {
                            "policy_mismatch"
                        };
                    return Err(InvalidNotModified(reason).into());
                }
            }
            let policy = candidate
                .then(|| cache::policy(&policy_request, &policy_response))
                .flatten();
            if candidate && policy.is_none() {
                let c = state.cache.clone();
                let k = key.clone().expect("cache candidate");
                let _ = tokio::task::spawn_blocking(move || c.remove(&k)).await;
            }
            let mut prefix = BytesMut::new();
            if let Some(policy) = policy {
                if response.content_length().unwrap_or(0) <= cache::MAX_OBJECT as u64 {
                    let mut complete = false;
                    diagnostic.phase = "response_body_buffered";
                    while let Some(chunk) = response.chunk().await? {
                        state
                            .metrics
                            .received
                            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        prefix.extend_from_slice(&chunk);
                        if prefix.len() > cache::MAX_OBJECT {
                            break;
                        }
                    }
                    if prefix.len() <= cache::MAX_OBJECT {
                        complete = true;
                    }
                    if complete {
                        let bytes = prefix.freeze();
                        let validation_bytes = bytes.clone();
                        let validation_headers = response_headers.clone();
                        diagnostic.phase = "cache_validation";
                        let asset_path = asset_path.clone();
                        if tokio::task::spawn_blocking(move || {
                            cache::valid_content(
                                &asset_path,
                                &validation_headers,
                                &validation_bytes,
                            )
                        })
                        .await?
                        {
                            let k = key.clone().expect("cache candidate");
                            let entry = Cached {
                                policy,
                                body: bytes.clone(),
                            };
                            state.persist_later(k, entry, true);
                        }
                        let mut out = Response::new(body(bytes));
                        *out.status_mut() = status;
                        *out.headers_mut() = response_headers;
                        return Ok(out);
                    }
                }
            }
            let metrics = state.metrics.clone();
            diagnostic.phase = "response_body_stream";
            let stream_diagnostic = diagnostic.clone();
            let stream_guards = guards.clone();
            let tail = response.bytes_stream().map(move |chunk| {
                let _keep_guards_until_stream_ends = &stream_guards;
                chunk
                    .map(|bytes| {
                        metrics
                            .received
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        Frame::data(bytes)
                    })
                    .map_err(|e| {
                        stream_diagnostic.failure(&e, Some(status.as_u16()));
                        Box::new(e) as Error
                    })
            });
            let initial = futures_util::stream::iter(if prefix.is_empty() {
                vec![]
            } else {
                vec![Ok(Frame::data(prefix.freeze()))]
            });
            let mut out =
                Response::new(BodyExt::boxed_unsync(StreamBody::new(initial.chain(tail))));
            *out.status_mut() = status;
            *out.headers_mut() = response_headers;
            Ok(out)
        };
        let result: Result<Response<Body>> = tokio::select! {
            _ = state.cancel.cancelled() => anyhow::bail!("proxy cancelled"),
            result = async {
                if let Some(until) = deadline { tokio::time::timeout_at(until, attempt).await? }
                else { attempt.await }
            } => result,
        };
        match result {
            Ok(response) => {
                if diagnostic.attempt > 1 && response.status().is_success() {
                    diagnostic.recovery(
                        "proxy_request_recovered",
                        if diagnostic.phase == "response_body_stream" {
                            "response_ready"
                        } else {
                            "success"
                        },
                    );
                }
                return Ok(response);
            }
            Err(error) => {
                let invalid = error.downcast_ref::<InvalidNotModified>();
                let retryable = candidate
                    && (invalid.is_some() || recoverable_transport(&error, diagnostic.phase));
                let until = tokio::time::Instant::now() + Duration::from_secs(5);
                if candidate && invalid.is_some() {
                    diagnostic.phase = "cache_invalidate";
                    let c = state.cache.clone();
                    let k = key.clone().expect("cache candidate");
                    // A timed-out spawn_blocking operation still runs. Retain the
                    // asset lock until removal finishes so it cannot delete a
                    // later request's replacement entry after this request exits.
                    let removal_guards = guards.clone();
                    tokio::select! {
                        _ = state.cancel.cancelled() => anyhow::bail!("proxy cancelled"),
                        result = tokio::time::timeout_at(deadline.unwrap_or(until), tokio::task::spawn_blocking(move || {
                            let _keep_asset_locked = removal_guards;
                            c.remove(&k)
                        })) => { result???; }
                    }
                    cached = None;
                    diagnostic.phase = "cache_revalidation";
                }
                if !retryable || diagnostic.attempt >= 2 || state.cancel.is_cancelled() {
                    return Err(error);
                }
                let reason = invalid.map(|e| e.0).unwrap_or_else(|| {
                    crate::proxy_diagnostics::classify(error.as_ref(), diagnostic.phase)
                });
                diagnostic.recovery("proxy_request_retry", reason);
                deadline = Some(until);
                if invalid.is_some() {
                    headers = original_headers.clone();
                    diagnostic.cache = "refetch";
                } else {
                    tokio::select! {
                        _ = state.cancel.cancelled() => anyhow::bail!("proxy cancelled"),
                        _ = tokio::time::sleep(Duration::from_millis(100)) => (),
                    }
                }
                diagnostic.attempt += 1;
            }
        }
    }
}
fn valid_destination(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']);
    if h.eq_ignore_ascii_case("localhost") || h.ends_with(".localhost") {
        return false;
    }
    !h.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}
pub fn strip_hop_headers(headers: &mut HeaderMap) {
    let named: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|s| s.trim().to_string()))
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn saved_downloads(state: &ContextState, count: u64) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while state.metrics.downloads.load(Ordering::Relaxed) < count {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("accepted persistence completes");
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn downloaded_response_and_followers_finish_before_disk_then_stop_drains() {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = source.local_addr().unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = source.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
            }
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nCache-Control: public, max-age=600\r\nContent-Type: application/javascript\r\nConnection: close\r\n\r\nconst x=1;").await.unwrap();
        });
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .resolve("prd-game-a-granbluefantasy.akamaized.net", address)
            .build()
            .unwrap();
        let (state, port, dir, listener) = test_proxy(Settings::default(), None, upstream).await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
            .build()
            .unwrap();
        let uri = "http://prd-game-a-granbluefantasy.akamaized.net/assets/cold.js";
        let request_client = client.clone();
        let request = tokio::spawn(async move {
            request_client
                .get(uri)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        });
        entered_rx.await.unwrap(); // The cold lookup completed; block only the subsequent write.
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (unlock_tx, unlock_rx) = std::sync::mpsc::channel();
        let cache = state.cache.clone();
        let disk = tokio::task::spawn_blocking(move || cache.hold_disk(held_tx, unlock_rx));
        tokio::task::spawn_blocking(move || held_rx.recv().unwrap())
            .await
            .unwrap();
        release_tx.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), request).await;
        let followers =
            tokio::time::timeout(
                Duration::from_secs(2),
                futures_util::future::join_all((0..8).map(|_| async {
                    client.get(uri).send().await.unwrap().bytes().await.unwrap()
                })),
            )
            .await;
        let saved_before_unlock = state.metrics.downloads.load(Ordering::Relaxed);
        // Always release the blocking worker before any assertion can unwind.
        unlock_tx.send(()).unwrap();
        disk.await.unwrap();
        assert_eq!(saved_before_unlock, 0);
        assert_eq!(result.unwrap().unwrap(), "const x=1;");
        for body in followers.unwrap() {
            assert_eq!(body, "const x=1;");
        }
        finish(&state, listener).await;
        assert_eq!(state.metrics.downloads.load(Ordering::Relaxed), 1);
        assert_eq!(
            Cache::open(dir.path())
                .unwrap()
                .get(&CacheKey::from_url(uri).unwrap())
                .unwrap()
                .unwrap()
                .body,
            "const x=1;"
        );
        origin.await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cache_hits_bypass_sixteen_blocked_downloads_and_disk_work() {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = source.local_addr().unwrap();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (entered, mut arrivals) = tokio::sync::mpsc::channel(16);
        let gate = release.clone();
        let origin = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..16 {
                let (mut stream, _) = source.accept().await.unwrap();
                let gate = gate.clone();
                let entered = entered.clone();
                tasks.spawn(async move {
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") { header.push(stream.read_u8().await.unwrap()); }
                    entered.send(()).await.unwrap();
                    let _permit = gate.acquire().await.unwrap();
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nCache-Control: public, max-age=600\r\nConnection: close\r\n\r\nx").await.unwrap();
                });
            }
            while tasks.join_next().await.is_some() {}
        });
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .resolve("prd-game-a-granbluefantasy.akamaized.net", address)
            .build()
            .unwrap();
        let (state, port, _dir, listener) = test_proxy(Settings::default(), None, upstream).await;
        let uri = "http://prd-game-a-granbluefantasy.akamaized.net/assets/cached.js";
        let request = Request::builder().uri(uri).body(()).unwrap();
        let response = Response::builder()
            .header("cache-control", "public, max-age=600")
            .body(())
            .unwrap();
        let key = CacheKey::from_url(uri).unwrap();
        state
            .cache
            .put(
                &key,
                cache::Cached {
                    policy: cache::policy(&request, &response).unwrap(),
                    body: bytes::Bytes::from_static(b"cached"),
                },
                1000,
            )
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
            .build()
            .unwrap();
        let mut downloads = tokio::task::JoinSet::new();
        for i in 0..16 {
            let client = client.clone();
            downloads.spawn(async move {
                client
                    .get(format!(
                        "http://prd-game-a-granbluefantasy.akamaized.net/assets/slow{i}.js"
                    ))
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
            });
        }
        for _ in 0..16 {
            tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(state.fetch_slots.available_permits(), 0);
        let start = std::time::Instant::now();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), client.get(uri).send())
                .await
                .unwrap()
                .unwrap()
                .text()
                .await
                .unwrap(),
            "cached"
        );
        println!("RAM hit with 16 blocked origins: {:?}", start.elapsed());
        state.cache.evict_ram();
        let start = std::time::Instant::now();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), client.get(uri).send())
                .await
                .unwrap()
                .unwrap()
                .text()
                .await
                .unwrap(),
            "cached"
        );
        println!("disk hit with 16 blocked origins: {:?}", start.elapsed());
        let (entered, barrier) = std::sync::mpsc::channel();
        let (unblock, release_disk) = std::sync::mpsc::channel();
        let cache = state.cache.clone();
        let disk = std::thread::spawn(move || cache.hold_disk(entered, release_disk));
        barrier.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), client.get(uri).send()).await;
        unblock.send(()).unwrap();
        disk.join().unwrap();
        assert_eq!(result.unwrap().unwrap().text().await.unwrap(), "cached");
        release.add_permits(16);
        while let Some(result) = downloads.join_next().await {
            result.unwrap();
        }
        origin.await.unwrap();
        finish(&state, listener).await;
    }
    #[test]
    fn strips_connection_named_headers() {
        let mut h = HeaderMap::new();
        h.insert("connection", "x-secret, keep-alive".parse().unwrap());
        h.insert("x-secret", "bad".parse().unwrap());
        h.insert("proxy-authorization", "bad".parse().unwrap());
        h.insert("content-type", "image/png".parse().unwrap());
        strip_hop_headers(&mut h);
        assert_eq!(h.len(), 1);
        assert!(h.contains_key("content-type"));
    }

    pub(crate) async fn test_proxy(
        settings: Settings,
        authority: Option<Authority>,
        upstream: reqwest::Client,
    ) -> (
        Arc<ContextState>,
        u16,
        tempfile::TempDir,
        tokio::task::JoinHandle<()>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let settings = Settings {
            listen_port: port,
            ..settings
        };
        let mut state = ContextState::new(
            settings,
            String::new(),
            Arc::new(Cache::open(dir.path()).unwrap()),
            authority,
            Arc::new(Metrics::default()),
        )
        .unwrap();
        state.upstream = upstream;
        let state = Arc::new(state);
        let task = tokio::spawn(serve(listener, state.clone()));
        (state, port, dir, task)
    }
    async fn finish(state: &ContextState, listener: tokio::task::JoinHandle<()>) {
        state.cancel.cancel();
        listener.await.unwrap();
        state.tasks.close();
        tokio::time::timeout(Duration::from_secs(2), state.tasks.wait())
            .await
            .unwrap();
        assert_eq!(state.metrics.connections.load(Ordering::Relaxed), 0);
    }
    pub(crate) async fn origin(
        tls: Option<Arc<rustls::ServerConfig>>,
    ) -> (
        std::net::SocketAddr,
        Arc<AtomicU64>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let count = count.clone();
                let tls = tls.clone();
                tokio::spawn(async move {
                    let stream: routing::BoxStream = if let Some(config) = tls {
                        Box::new(
                            tokio_rustls::TlsAcceptor::from(config)
                                .accept(stream)
                                .await
                                .unwrap(),
                        )
                    } else {
                        Box::new(stream)
                    };
                    let service = service_fn(move |req: Request<Incoming>| {
                        let count = count.clone();
                        async move {
                            count.fetch_add(1, Ordering::Relaxed);
                            if req.uri().path().contains("/benchmark/") {
                                assert!(!req.headers().contains_key("cookie"));
                                assert!(!req.headers().contains_key("authorization"));
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                            if req.uri().path().contains("/slow-response/") {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                            }
                            let mut res = Response::builder().header("etag", "\"version-1\"");
                            if req.uri().path().contains("no-control") {
                                // Deliberately omit Cache-Control on BOTH 200 and 304.
                            } else if req.uri().path().contains("private") {
                                res = res.header("cache-control", "private, max-age=600");
                            } else if req.uri().path().contains("stale")
                                && !req.headers().contains_key("if-none-match")
                            {
                                res = res.header("cache-control", "public, max-age=0");
                            } else {
                                res = res.header("cache-control", "public, max-age=600");
                            }
                            if req.headers().contains_key("if-none-match") {
                                res = res.status(304);
                            }
                            let res = res
                                .body(Full::new(if req.headers().contains_key("if-none-match") {
                                    Bytes::new()
                                } else {
                                    Bytes::from_static(
                                        if req.uri().path().starts_with("/assets_en/") {
                                            b"english-bytes"
                                        } else {
                                            b"asset-bytes"
                                        },
                                    )
                                }))
                                .unwrap();
                            Ok::<_, Infallible>(res)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        (addr, calls, task)
    }
    #[tokio::test]
    async fn pac_cache_revalidation_private_and_concurrent_misses() {
        let (addr, count, source) = origin(None).await;
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
            .build()
            .unwrap();
        let (state, port, _dir, listener) = test_proxy(Settings::default(), None, upstream).await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
            .build()
            .unwrap();
        let pac = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://127.0.0.1:{port}/proxy.pac"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(pac.contains(&format!("PROXY 127.0.0.1:{port}'")));
        assert!(!pac.contains("; DIRECT"));
        let url = "http://prd-game-a-granbluefantasy.akamaized.net/assets/test.js?v=1";
        let results = futures_util::future::join_all((0..8).map(|_| client.get(url).send())).await;
        for res in results {
            assert_eq!(res.unwrap().text().await.unwrap(), "asset-bytes");
        }
        assert_eq!(count.load(Ordering::Relaxed), 1);
        saved_downloads(&state, 1).await;
        assert_eq!(state.metrics.downloads.load(Ordering::Relaxed), 1);
        assert_eq!(state.metrics.hits.load(Ordering::Relaxed), 7);
        for _ in 0..2 {
            let res = client
                .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/private.js")
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 200);
            let _ = res.bytes().await.unwrap();
        }
        assert_eq!(count.load(Ordering::Relaxed), 3);
        for _ in 0..3 {
            assert_eq!(
                client
                    .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/stale.js")
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                "asset-bytes"
            );
        }
        assert_eq!(count.load(Ordering::Relaxed), 5);
        // Version query changes are distinct resources.
        assert_eq!(
            client
                .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/test.js?v=2")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(count.load(Ordering::Relaxed), 6);
        for _ in 0..3 {
            assert_eq!(
                client
                    .get("http://prd-game-a-granbluefantasy.akamaized.net/assets/no-control.js")
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                "asset-bytes"
            );
        }
        assert_eq!(count.load(Ordering::Relaxed), 9);
        finish(&state, listener).await;
        source.abort();
    }
    #[tokio::test]
    async fn https_asset_cache_with_ephemeral_ca_without_installing_trust() {
        let origin_ca = Authority::ephemeral();
        let origin_pem = origin_ca.pem();
        let (addr, count, source) = origin(Some(
            origin_ca
                .server("prd-game-a-granbluefantasy.akamaized.net")
                .unwrap(),
        ))
        .await;
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(origin_pem.as_bytes()).unwrap())
            .resolve("prd-game-a-granbluefantasy.akamaized.net", addr)
            .build()
            .unwrap();
        let local_ca = Authority::ephemeral();
        let local_pem = local_ca.pem();
        let (state, port, _dir, listener) =
            test_proxy(Settings::default(), Some(local_ca), upstream).await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(local_pem.as_bytes()).unwrap())
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
            .build()
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                client
                    .get("https://prd-game-a-granbluefantasy.akamaized.net/assets/test.js")
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                "asset-bytes"
            );
        }
        let english = "https://prd-game-a-granbluefantasy.akamaized.net/assets_en/test.js";
        let results =
            futures_util::future::join_all((0..6).map(|_| client.get(english).send())).await;
        for result in results {
            assert_eq!(result.unwrap().text().await.unwrap(), "english-bytes");
        }
        assert_eq!(count.load(Ordering::Relaxed), 2);
        saved_downloads(&state, 2).await;
        assert_eq!(state.metrics.downloads.load(Ordering::Relaxed), 2);
        assert_eq!(
            std::fs::read_dir(state.cache.root().join("ja"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_dir(state.cache.root().join("en"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(state.metrics.requests.load(Ordering::Relaxed), 8);
        assert_eq!(state.metrics.hits.load(Ordering::Relaxed), 6);
        finish(&state, listener).await;
        source.abort();
    }
    #[tokio::test]
    async fn api_connect_is_opaque_and_stop_closes_tunnel() {
        for host in std::iter::once("game.granbluefantasy.jp")
            .chain(rules::TUNNEL_HOSTS.iter().copied())
            .chain(["login.mobage.jp", "a.b.dmm.com"])
        {
            let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let upstream_port = upstream_listener.local_addr().unwrap().port();
            let peer = tokio::spawn(async move {
                let (mut stream, _) = upstream_listener.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    header.push(stream.read_u8().await.unwrap());
                }
                assert!(String::from_utf8(header)
                    .unwrap()
                    .starts_with(&format!("CONNECT {host}:443 HTTP/1.1")));
                stream
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .unwrap();
                let mut opaque = [0u8; 8];
                stream.read_exact(&mut opaque).await.unwrap();
                assert_eq!(&opaque, b"TLSBYTES");
                stream.write_all(&opaque).await.unwrap();
            });
            let settings = Settings {
                mode: crate::config::Mode::Http,
                upstream_port,
                ..Default::default()
            };
            let (state, port, _dir, listener) = test_proxy(
                settings,
                Some(Authority::ephemeral()),
                reqwest::Client::new(),
            )
            .await;
            let mut browser = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            browser
                .write_all(
                    format!("CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(browser.read_u8().await.unwrap());
            }
            assert!(String::from_utf8(header)
                .unwrap()
                .starts_with("HTTP/1.1 200"));
            browser.write_all(b"TLSBYTES").await.unwrap();
            let mut reply = [0u8; 8];
            browser.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"TLSBYTES");
            assert_eq!(state.metrics.requests.load(Ordering::Relaxed), 1);
            assert_eq!(state.metrics.downloads.load(Ordering::Relaxed), 0);
            assert_eq!(state.metrics.sent.load(Ordering::Relaxed), 8);
            peer.await.unwrap();
            finish(&state, listener).await;
        }
    }
}
