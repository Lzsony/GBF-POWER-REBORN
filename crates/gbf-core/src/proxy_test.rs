use super::*;
use tokio_util::sync::CancellationToken;

pub(super) struct Job {
    id: String,
    cancel: CancellationToken,
    done: CancellationToken,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    pub test_id: String,
    pub median_ms: Option<f64>,
    pub timeout_percent: Option<f64>,
    pub sample_count: usize,
    pub success_count: usize,
    pub completed_at: u64,
    pub state: &'static str,
}
impl ProxyTestResult {
    fn empty(test_id: String) -> Self {
        Self {
            test_id,
            median_ms: None,
            timeout_percent: None,
            sample_count: 0,
            success_count: 0,
            completed_at: 0,
            state: "failed",
        }
    }
}
impl Runtime {
    pub async fn cancel_proxy_test(&self, id: Option<&str>) {
        let done = {
            let slot = self.proxy_test.lock().await;
            slot.as_ref()
                .filter(|j| id.is_none_or(|id| id == j.id))
                .map(|j| {
                    j.cancel.cancel();
                    j.done.clone()
                })
        };
        if let Some(done) = done {
            done.cancelled().await;
        }
    }
    pub async fn test_proxy_port(
        self: &Arc<Self>,
        test_id: String,
        settings: Settings,
    ) -> ProxyTestResult {
        self.run_manual_test(test_id, settings).await
    }
    async fn run_manual_test(
        self: &Arc<Self>,
        test_id: String,
        settings: Settings,
    ) -> ProxyTestResult {
        let failure = ProxyTestResult::empty(test_id.clone());
        if test_id.is_empty()
            || test_id.len() > 128
            || !test_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            return failure;
        }
        let operation = self.operations.lock().await;
        if self.control_state().shutting_down || self.ensure_no_audit().is_err() {
            return failure;
        }
        let mut slot = self.proxy_test.lock().await;
        if slot.as_ref().is_some_and(|j| j.done.is_cancelled()) {
            slot.take();
        }
        if slot.is_some() {
            return failure;
        }
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        *slot = Some(Job {
            id: test_id.clone(),
            cancel: cancel.clone(),
            done: done.clone(),
        });
        drop(slot);
        let core = self.clone();
        let task = tokio::spawn(async move {
            let _completion = done.clone().drop_guard();
            let result = tokio::select! {
                _=cancel.cancelled()=>{let mut r=failure.clone();r.state="cancelled";r},
                r=tokio::time::timeout(Duration::from_secs(45),core.manual_worker(&test_id,settings))=>r.unwrap_or(failure),
            };
            cancel.cancel();
            core.proxy_test.lock().await.take();
            done.cancel();
            let mut result = result;
            result.completed_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            result
        });
        drop(operation);
        task.await
            .unwrap_or_else(|_| ProxyTestResult::empty(String::new()))
    }
    async fn manual_worker(&self, test_id: &str, settings: Settings) -> ProxyTestResult {
        let mut r = ProxyTestResult::empty(test_id.into());
        if !matches!(settings.mode, Mode::Http | Mode::Socks5) || settings.validate().is_err() {
            return r;
        }
        let reachable = tokio::time::timeout(Duration::from_secs(5), async {
            let addresses =
                crate::tcp_probe::resolve(&settings.upstream_host, settings.upstream_port).await;
            if addresses
                .iter()
                .any(|a| a.ip().is_loopback() && a.port() == settings.listen_port)
            {
                return false;
            }
            matches!(
                crate::tcp_probe::sample(&addresses).await,
                ProbeSample::Success(_)
            )
        })
        .await
        .unwrap_or(false);
        r.sample_count = 1;
        r.success_count = usize::from(reachable);
        r.state = if reachable { "success" } else { "failed" };
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt;
    async fn fixture() -> (
        tempfile::TempDir,
        Arc<Runtime>,
        u16,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = accepted.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = socket.accept().await.unwrap();
                // It is deliberately not an HTTP or SOCKS server. A test must send no data.
                let mut b = [0; 1];
                assert_eq!(stream.read(&mut b).await.unwrap(), 0);
                count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let core = Arc::new(Runtime::new(dir.path().into()).unwrap());
        (dir, core, port, accepted, task)
    }
    async fn wait_count(count: &AtomicUsize, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while count.load(Ordering::Relaxed) != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn proxy_port_test_accepts_non_proxy_listener_without_saving_or_measuring_latency() {
        let (dir, core, port, count, server) = fixture().await;
        let before = std::fs::read(dir.path().join("config.json")).unwrap();
        let settings = Settings {
            mode: Mode::Socks5,
            upstream_host: "127.0.0.1".into(),
            upstream_port: port,
            ..Default::default()
        };
        let result = core.test_proxy_port("port-one".into(), settings).await;
        assert_eq!(result.state, "success");
        assert_eq!(result.median_ms, None);
        assert_eq!(result.sample_count, 1);
        assert_eq!(
            before,
            std::fs::read(dir.path().join("config.json")).unwrap()
        );
        wait_count(&count, 1).await;
        assert!(core.active.lock().await.is_none());
        server.abort();
    }
    #[tokio::test]
    async fn running_proxy_remains_active_during_port_test() {
        let (dir, core, port, _, server) = fixture().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen_port = listener.local_addr().unwrap().port();
        let settings = Settings {
            listen_port,
            ..Default::default()
        };
        let context = Arc::new(
            ContextState::new(
                settings,
                String::new(),
                core.cache.clone(),
                None,
                core.metrics.read().unwrap().clone(),
            )
            .unwrap(),
        );
        let task = tokio::spawn(proxy::serve(listener, context.clone()));
        *core.active.lock().await = Some(Running {
            context,
            listener: task,
        });
        *core.started.write().unwrap() = Some(Instant::now());
        let before = std::fs::read(dir.path().join("config.json")).unwrap();
        let settings = Settings {
            mode: Mode::Http,
            upstream_host: "127.0.0.1".into(),
            upstream_port: port,
            listen_port,
            ..Default::default()
        };
        assert_eq!(
            core.test_proxy_port("running-port".into(), settings)
                .await
                .state,
            "success"
        );
        assert!(core.active.lock().await.is_some());
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", listen_port))
            .await
            .is_ok());
        assert_eq!(
            before,
            std::fs::read(dir.path().join("config.json")).unwrap()
        );
        core.shutdown().await.unwrap();
        server.abort();
    }
    #[tokio::test]
    async fn cancellation_and_shutdown_close_job_admission() {
        let (_dir, core, _, _, server) = fixture().await;
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        *core.proxy_test.lock().await = Some(Job {
            id: "held".into(),
            cancel: cancel.clone(),
            done: done.clone(),
        });
        let cleanup = tokio::spawn(async move {
            cancel.cancelled().await;
            done.cancel();
        });
        core.cancel_proxy_test(Some("held")).await;
        cleanup.await.unwrap();
        core.shutdown().await.unwrap();
        assert_eq!(
            core.test_proxy_port("after-quit".into(), Settings::default())
                .await
                .state,
            "failed"
        );
        server.abort();
    }
    #[tokio::test]
    async fn loopback_proxy_loop_is_rejected_without_connecting() {
        let (_dir, core, port, count, server) = fixture().await;
        let settings = Settings {
            mode: Mode::Http,
            upstream_host: "localhost".into(),
            upstream_port: port,
            listen_port: port,
            ..Default::default()
        };
        assert_eq!(
            core.test_proxy_port("loop".into(), settings).await.state,
            "failed"
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
        server.abort();
    }
}
