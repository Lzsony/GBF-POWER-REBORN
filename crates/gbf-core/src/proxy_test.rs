use super::*;
use futures_util::{stream, StreamExt};
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
    pub line_id: String,
    pub revision: String,
    pub median_ms: Option<f64>,
    pub timeout_percent: Option<f64>,
    pub sample_count: usize,
    pub success_count: usize,
    pub completed_at: u64,
    pub state: &'static str,
}
impl ProxyTestResult {
    fn empty(test_id: String, line_id: String) -> Self {
        Self {
            test_id,
            line_id,
            revision: String::new(),
            median_ms: None,
            timeout_percent: None,
            sample_count: 0,
            success_count: 0,
            completed_at: 0,
            state: "failed",
        }
    }
}
enum Work {
    Lines(Vec<acceleration::Line>),
    Port(Settings),
}
impl Runtime {
    fn test_line_config(&self, id: &str) -> Result<acceleration::Line> {
        self.authorization.line(id)
    }
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
    pub async fn test_line(self: &Arc<Self>, test_id: String, line_id: String) -> ProxyTestResult {
        let lines = self
            .test_line_config(&line_id)
            .map(|l| vec![l])
            .unwrap_or_default();
        self.run_manual_test(test_id, line_id, Work::Lines(lines))
            .await
    }
    pub async fn test_auto_lines(self: &Arc<Self>, test_id: String) -> ProxyTestResult {
        let lines = self
            .authorization
            .available_lines()
            .iter()
            .filter_map(|(id, _)| self.test_line_config(id).ok())
            .collect();
        self.run_manual_test(test_id, String::new(), Work::Lines(lines))
            .await
    }
    pub async fn test_proxy_port(
        self: &Arc<Self>,
        test_id: String,
        settings: Settings,
    ) -> ProxyTestResult {
        self.run_manual_test(test_id, String::new(), Work::Port(settings))
            .await
    }
    async fn run_manual_test(
        self: &Arc<Self>,
        test_id: String,
        line_id: String,
        work: Work,
    ) -> ProxyTestResult {
        let failure = ProxyTestResult::empty(test_id.clone(), line_id);
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
                r=tokio::time::timeout(Duration::from_secs(45),core.manual_worker(&test_id,work,&cancel))=>r.unwrap_or(failure),
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
            .unwrap_or_else(|_| ProxyTestResult::empty(String::new(), String::new()))
    }
    async fn manual_worker(
        &self,
        test_id: &str,
        work: Work,
        cancel: &CancellationToken,
    ) -> ProxyTestResult {
        match work {
            Work::Port(settings) => {
                let mut r = ProxyTestResult::empty(test_id.into(), String::new());
                if !matches!(settings.mode, Mode::Http | Mode::Socks5)
                    || settings.validate().is_err()
                {
                    return r;
                }
                let reachable = tokio::time::timeout(Duration::from_secs(5), async {
                    let addresses =
                        crate::tcp_probe::resolve(&settings.upstream_host, settings.upstream_port)
                            .await;
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
            Work::Lines(lines) => {
                let mut candidates=stream::iter(lines.into_iter().map(|line|async move {
                    let mut r=ProxyTestResult::empty(test_id.into(),line.id.clone());r.revision=line.revision(&self.root);
                    let invalidated=async {loop {
                        if self.test_line_config(&line.id).map_or(true,|l|l.revision(&self.root)!=r.revision){return;}
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }};
                    let batch=tokio::select!{_ = invalidated=>None, b=crate::tcp_probe::batch(&line.host,line.port,cancel)=>Some(b)};
                    if let Some(batch)=batch {
                        r.sample_count=batch.samples.len();r.success_count=batch.samples.iter().filter(|s|s.latency_ms().is_some()).count();r.timeout_percent=batch.timeout_percent();
                        if r.success_count>=2 {r.median_ms=batch.median();r.state="success";}
                    }else{r.state="cancelled";}
                    r
                })).buffer_unordered(2);
                let mut best = None;
                let mut last = ProxyTestResult::empty(test_id.into(), String::new());
                while let Some(result) = candidates.next().await {
                    if result.state == "success" {
                        let score = (
                            result.sample_count - result.success_count,
                            (result.median_ms.unwrap() * 1000.) as u64,
                            result.line_id.clone(),
                        );
                        if best.as_ref().is_none_or(|(old, _)| score < *old) {
                            best = Some((score, result));
                        }
                    } else {
                        last = result;
                    }
                }
                best.map(|(_, r)| r).unwrap_or(last)
            }
        }
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
                // It is deliberately not an SSH, HTTP or SOCKS server. A test must send no data.
                let mut b = [0; 1];
                assert_eq!(stream.read(&mut b).await.unwrap(), 0);
                count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let core = Arc::new(
            Runtime::new_with_profile(
                dir.path().into(),
                Some(crate::authorization::test_profile("tests")),
            )
            .unwrap(),
        );
        let lines = vec![acceleration::Line {
            id: "tcp-test".into(),
            name: "TCP".into(),
            host: "127.0.0.1".into(),
            port,
            username: "reborn".into(),
            identity_file: "ssh/nonexistent".into(),
            known_hosts_file: "ssh/nonexistent-host".into(),
        }];
        core.authorization.set_test_lines(lines).unwrap();
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
    async fn tcp_line_measurement_needs_no_ssh_key_and_does_not_start_proxy() {
        let (dir, core, _, count, server) = fixture().await;
        let before = std::fs::read(dir.path().join("config.json")).unwrap();
        let result = core.test_line("tcp-one".into(), "tcp-test".into()).await;
        assert_eq!(result.state, "success");
        assert_eq!(result.sample_count, 3);
        assert_eq!(result.success_count, 3);
        assert!(result.median_ms.is_some());
        wait_count(&count, 3).await;
        assert!(core.active.lock().await.is_none());
        assert_eq!(
            before,
            std::fs::read(dir.path().join("config.json")).unwrap()
        );
        assert_eq!(
            core.metrics
                .read()
                .unwrap()
                .requests
                .load(Ordering::Relaxed),
            0
        );
        server.abort();
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
    async fn automatic_test_skips_unreachable_candidate_and_keeps_configuration() {
        let (dir, core, _, _, server) = fixture().await;
        let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = reserved.local_addr().unwrap().port();
        drop(reserved);
        let first = core.authorization.line("tcp-test").unwrap();
        let mut other = first.clone();
        other.id = "unreachable".into();
        other.port = closed;
        core.authorization
            .set_test_lines(vec![first, other])
            .unwrap();
        let before = std::fs::read(dir.path().join("config.json")).unwrap();
        let result = core.test_auto_lines("auto-one".into()).await;
        assert_eq!(result.state, "success");
        assert_eq!(result.line_id, "tcp-test");
        assert_eq!(
            before,
            std::fs::read(dir.path().join("config.json")).unwrap()
        );
        assert!(core.active.lock().await.is_none());
        server.abort();
    }
    #[tokio::test]
    async fn running_proxy_remains_active_during_port_and_proxy_tests() {
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
            tunnel: None,
            context,
            listener: task,
        });
        *core.started.write().unwrap() = Some(Instant::now());
        let before = std::fs::read(dir.path().join("config.json")).unwrap();
        assert_eq!(
            core.test_line("running-line".into(), "tcp-test".into())
                .await
                .state,
            "success"
        );
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
            core.test_line("after-quit".into(), "tcp-test".into())
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
