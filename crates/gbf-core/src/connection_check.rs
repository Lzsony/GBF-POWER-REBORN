//! Nonblocking TCP checks bound to the actual SSH session, never a candidate route.
use super::{acceleration, ErrorCode, ProxyTestResult, Runtime};
use crate::{metrics::ProbeSample, probe::Batch};
use anyhow::{ensure, Result};
use serde::Serialize;
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCheckView {
    pub connection_id: Option<String>,
    pub result: Option<ProxyTestResult>,
}

struct Binding {
    line: acceleration::Line,
    revision: String,
    session: CancellationToken,
}
struct Job {
    result: ProxyTestResult,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
#[derive(Default)]
struct State {
    binding: Option<Binding>,
    connection_id: Option<String>,
    job: Option<Job>,
}
impl State {
    fn clear(&mut self) {
        self.job.take();
        self.binding = None;
        self.connection_id = None;
    }
    fn view(&mut self) -> ConnectionCheckView {
        if self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.session.is_cancelled())
        {
            self.clear();
        }
        ConnectionCheckView {
            connection_id: self.connection_id.clone(),
            result: self.job.as_ref().map(|job| job.result.clone()),
        }
    }
}

#[derive(Default)]
pub(super) struct Checks(Arc<Mutex<State>>);
impl Drop for Checks {
    fn drop(&mut self) {
        self.0.lock().unwrap().clear();
    }
}
impl Checks {
    pub(super) fn reset(&self) {
        self.0.lock().unwrap().clear();
    }
    fn bind(&self, binding: Option<Binding>) {
        let mut state = self.0.lock().unwrap();
        if let (Some(old), Some(new)) = (&state.binding, &binding) {
            if old.session == new.session
                && old.line.id == new.line.id
                && old.revision == new.revision
                && !old.session.is_cancelled()
            {
                return;
            }
        }
        state.clear();
        if let Some(binding) = binding.filter(|binding| !binding.session.is_cancelled()) {
            use rand_core::RngCore;
            let mut nonce = [0; 16];
            rand_core::OsRng.fill_bytes(&mut nonce);
            state.connection_id = Some(nonce.iter().map(|byte| format!("{byte:02x}")).collect());
            state.binding = Some(binding);
        }
    }
    fn view(&self) -> ConnectionCheckView {
        self.0.lock().unwrap().view()
    }
    fn cancel(&self, id: &str) -> ConnectionCheckView {
        let mut state = self.0.lock().unwrap();
        if state
            .job
            .as_ref()
            .is_some_and(|job| job.result.test_id == id)
        {
            state.job.take();
        }
        state.view()
    }
    fn start(&self, id: String) -> Result<ConnectionCheckView> {
        self.start_with(id, |host, port, cancel| async move {
            crate::tcp_probe::batch(&host, port, &cancel).await
        })
    }
    fn start_with<F, Fut>(&self, id: String, measure: F) -> Result<ConnectionCheckView>
    where
        F: FnOnce(String, u16, CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Batch> + Send + 'static,
    {
        ensure!(
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
            ErrorCode::InvalidCommand
        );
        let mut state = self.0.lock().unwrap();
        let view = state.view();
        ensure!(view.connection_id.is_some(), ErrorCode::SshConnectionFailed);
        ensure!(
            !state
                .job
                .as_ref()
                .is_some_and(|job| job.result.state == "running"),
            ErrorCode::ConnectionTestFailed
        );
        state.job.take();
        let binding = state.binding.as_ref().unwrap();
        let session = binding.session.clone();
        let cancel = session.child_token();
        let mut result = ProxyTestResult::empty(id, binding.line.id.clone());
        result.revision = binding.revision.clone();
        result.state = "running";
        let host = binding.line.host.clone();
        let port = binding.line.port;
        let work = cancel.clone();
        let shared = self.0.clone();
        let mut completed = result.clone();
        let task = tokio::spawn(async move {
            let batch = tokio::select! {
                biased;
                _ = work.cancelled() => return,
                batch = tokio::time::timeout(Duration::from_secs(9), measure(host, port, work.clone())) => batch.unwrap_or(Batch { samples: vec![ProbeSample::Timeout] }),
            };
            completed.sample_count = batch.samples.len();
            completed.success_count = batch
                .samples
                .iter()
                .filter(|s| s.latency_ms().is_some())
                .count();
            completed.timeout_percent = batch.timeout_percent();
            completed.state = "failed";
            if completed.success_count >= 2 {
                completed.median_ms = batch.median();
                completed.state = "success";
            }
            completed.completed_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let mut state = shared.lock().unwrap();
            if work.is_cancelled() || state.connection_id != view.connection_id {
                return;
            }
            if let Some(job) = state
                .job
                .as_mut()
                .filter(|job| job.result.test_id == completed.test_id)
            {
                job.result = completed;
            }
        });
        state.job = Some(Job {
            result,
            cancel,
            task,
        });
        Ok(state.view())
    }
}

impl Runtime {
    async fn bind_connection_check(&self) {
        let active = self.active.lock().await;
        let binding = active.as_ref().and_then(|running| {
            let tunnel = running.tunnel.as_ref()?;
            if self.acceleration_status().state != acceleration::Phase::Connected {
                return None;
            }
            let authorized = self.authorization.line(&tunnel.line.id).ok()?;
            let revision = tunnel.line.revision(&self.root);
            if authorized.revision(&self.root) != revision {
                return None;
            }
            Some(Binding {
                line: tunnel.line.clone(),
                revision,
                session: tunnel.session()?,
            })
        });
        self.connection_checks.bind(binding);
    }
    pub async fn connection_check_status(&self) -> ConnectionCheckView {
        self.bind_connection_check().await;
        self.connection_checks.view()
    }
    pub async fn start_connection_check(&self, id: String) -> Result<ConnectionCheckView> {
        let _operation = self.operations.lock().await;
        ensure!(
            !self.control_state().shutting_down,
            ErrorCode::SshConnectionFailed
        );
        self.bind_connection_check().await;
        self.connection_checks.start(id)
    }
    pub async fn cancel_connection_check(&self, id: &str) -> ConnectionCheckView {
        self.bind_connection_check().await;
        self.connection_checks.cancel(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bind(checks: &Checks, token: CancellationToken, port: u16) {
        checks.bind(Some(Binding {
            line: acceleration::Line {
                id: "selected-node".into(),
                name: "test".into(),
                host: "127.0.0.1".into(),
                port,
                username: "reborn".into(),
                identity_file: "key".into(),
                known_hosts_file: "known".into(),
            },
            revision: "revision".into(),
            session: token,
        }));
    }
    async fn finished(checks: &Checks) -> ProxyTestResult {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(result) = checks
                    .view()
                    .result
                    .filter(|result| result.state != "running")
                {
                    return result;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn probes_only_bound_node_and_preserves_live_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let checks = Checks::default();
        let token = CancellationToken::new();
        bind(
            &checks,
            token.clone(),
            listener.local_addr().unwrap().port(),
        );
        checks.start("test".into()).unwrap();
        let result = finished(&checks).await;
        assert_eq!(result.line_id, "selected-node");
        assert_eq!(result.success_count, 3);
        assert_eq!(result.state, "success");
        assert!(result.median_ms.is_some());
        assert!(!token.is_cancelled());
    }
    #[tokio::test]
    async fn requires_two_successes_and_never_reports_failed_zero_latency() {
        for (samples, expected) in [
            (
                vec![
                    ProbeSample::Success(12),
                    ProbeSample::Timeout,
                    ProbeSample::Success(20),
                ],
                Some(16.0),
            ),
            (
                vec![
                    ProbeSample::Success(0),
                    ProbeSample::Failure,
                    ProbeSample::Timeout,
                ],
                None,
            ),
            (vec![ProbeSample::Timeout; 3], None),
        ] {
            let checks = Checks::default();
            bind(&checks, CancellationToken::new(), 1);
            checks
                .start_with("id".into(), |_, _, _| async move { Batch { samples } })
                .unwrap();
            assert_eq!(finished(&checks).await.median_ms, expected);
        }
    }
    #[tokio::test]
    async fn cancellation_reconnection_and_duplicate_requests_do_not_publish_stale_results() {
        let checks = Checks::default();
        let token = CancellationToken::new();
        bind(&checks, token.clone(), 1);
        let first = checks
            .start_with("first".into(), |_, _, _| std::future::pending())
            .unwrap();
        assert!(checks.start("duplicate".into()).is_err());
        assert!(checks.cancel("other").result.is_some());
        assert!(checks.cancel("first").result.is_none());
        checks
            .start_with("second".into(), |_, _, _| std::future::pending())
            .unwrap();
        token.cancel();
        assert!(checks.view().connection_id.is_none());
        bind(&checks, CancellationToken::new(), 1);
        assert_ne!(checks.view().connection_id, first.connection_id);
        assert!(checks.view().result.is_none());
        checks
            .start_with("third".into(), |_, _, _| std::future::pending())
            .unwrap();
        checks.bind(None);
        assert!(checks.view().result.is_none());
        assert!(checks.start("disconnected".into()).is_err());
    }
    #[tokio::test]
    async fn runtime_rejects_check_without_an_authorized_active_tunnel() {
        let root = tempfile::tempdir().unwrap();
        let core = Runtime::new(root.path().into()).unwrap();
        assert!(core.start_connection_check("id".into()).await.is_err());
        assert!(core.connection_check_status().await.connection_id.is_none());
    }
}
