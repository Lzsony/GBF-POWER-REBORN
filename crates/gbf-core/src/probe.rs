//! Public-page probes, never game requests. Callers own cancellation and routing.
use crate::metrics::ProbeSample;
use std::time::Duration;
use tokio::time::Instant;

pub const GAME: &str = "https://game.granbluefantasy.jp/";
pub const STEAM: &str = "https://steam.granbluefantasy.com/";
pub const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Observation {
    pub sample: ProbeSample,
    pub elapsed_ms: u64,
    pub http_status: Option<u16>,
}
pub async fn head(client: &reqwest::Client, url: &str, timeout: Duration) -> Observation {
    let started = Instant::now();
    let response = client.head(url).timeout(timeout).send().await;
    let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let (sample, http_status) = match response {
        Ok(response) => (
            if response.status().is_success() {
                ProbeSample::Success(elapsed_ms)
            } else {
                ProbeSample::Failure
            },
            Some(response.status().as_u16()),
        ),
        Err(error) if error.is_timeout() => (ProbeSample::Timeout, None),
        Err(_) => (ProbeSample::Failure, None),
    };
    Observation {
        sample,
        elapsed_ms,
        http_status,
    }
}
impl Observation {
    pub fn log(&self, phase: &'static str) {
        tracing::info!(phase, elapsed_ms = self.elapsed_ms, http_status = self.http_status,
            outcome = ?self.sample, "public_page_probe");
    }
}

pub struct Batch {
    pub samples: Vec<ProbeSample>,
}
impl Batch {
    pub fn median(&self) -> Option<f64> {
        let mut good: Vec<_> = self.samples.iter().filter_map(|s| s.latency_ms()).collect();
        good.sort_unstable();
        let n = good.len();
        (n > 0).then(|| (good[(n - 1) / 2] as f64 + good[n / 2] as f64) / 2.0)
    }
    pub fn timeout_percent(&self) -> Option<f64> {
        (!self.samples.is_empty()).then(|| {
            self.samples
                .iter()
                .filter(|s| matches!(s, ProbeSample::Timeout))
                .count() as f64
                * 100.0
                / self.samples.len() as f64
        })
    }
}
pub async fn batch(client: &reqwest::Client, url: &str) -> Batch {
    let first = head(client, url, DEADLINE).await;
    first.log("initial");
    let mut samples = Vec::new();
    if matches!(first.sample, ProbeSample::Success(_)) {
        for _ in 0..3 {
            let next = head(client, url, DEADLINE).await;
            next.log("subsequent");
            samples.push(next.sample);
        }
    }
    Batch { samples }
}

pub async fn monitor(context: std::sync::Arc<crate::proxy::ContextState>, target: usize) {
    let url = if target == 2 { STEAM } else { GAME };
    loop {
        context.metrics.reset_probe(target);
        let Ok(client) = crate::routing::client(&context.settings, "", false) else {
            return;
        };
        let mut initial = true;
        let mut last_tick = Instant::now();
        let mut last_wall = std::time::SystemTime::now();
        let mut last_log = Instant::now() - Duration::from_secs(60);
        let mut last_outcome = None;
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { biased;
                _ = context.cancel.cancelled() => return,
                _ = interval.tick() => {}
            }
            let wall = std::time::SystemTime::now();
            if last_tick.elapsed() > Duration::from_secs(30)
                || wall
                    .duration_since(last_wall)
                    .map_or(true, |d| d > Duration::from_secs(30))
            {
                break;
            }
            last_tick = Instant::now();
            last_wall = wall;
            let result = tokio::select! { biased;
                _ = context.cancel.cancelled() => return,
                result = head(&client, url, DEADLINE) => result
            };
            let outcome = (std::mem::discriminant(&result.sample), result.http_status);
            if context.cancel.is_cancelled() {
                return;
            }
            if last_outcome != Some(outcome) || last_log.elapsed() >= Duration::from_secs(60) {
                result.log(if initial {
                    "monitor_initial"
                } else {
                    "monitor_subsequent"
                });
                last_outcome = Some(outcome);
                last_log = Instant::now();
            }
            if !initial {
                context.metrics.record_probe(target, result.sample);
            }
            initial = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    #[tokio::test]
    async fn initial_http_errors_never_follow_redirects_or_create_followups() {
        for status in [302, 403, 500] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(stream.read_u8().await.unwrap());
                }
                assert!(!String::from_utf8_lossy(&bytes)
                    .to_lowercase()
                    .contains("cookie:"));
                stream.write_all(format!("HTTP/1.1 {status} Response\r\nLocation: /again\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                listener
            });
            let client =
                crate::routing::client(&crate::config::Settings::default(), "", false).unwrap();
            let batch = batch(&client, &url).await;
            assert!(batch.samples.is_empty());
            assert_eq!(batch.median(), None);
            let listener = server.await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(30), listener.accept())
                    .await
                    .is_err()
            );
        }
    }
    #[tokio::test]
    async fn batch_excludes_initial_and_retains_http_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            // Explicitly close connections: subsequent does not promise reuse.
            for status in [204, 200, 503, 200] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(socket.read_u8().await.unwrap());
                }
                assert!(bytes.starts_with(b"HEAD / HTTP/1.1"));
                socket.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            }
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = batch(&client, &url).await;
        assert_eq!(result.samples.len(), 3);
        assert_eq!(result.samples[1], ProbeSample::Failure);
        assert!(result.median().is_some());
        assert_eq!(result.timeout_percent(), Some(0.0));
        server.await.unwrap();
    }
    #[test]
    fn failures_are_not_timeouts_and_even_median_is_averaged() {
        let batch = Batch {
            samples: vec![
                ProbeSample::Success(10),
                ProbeSample::Timeout,
                ProbeSample::Success(21),
                ProbeSample::Failure,
            ],
        };
        assert_eq!(batch.median(), Some(15.5));
        assert_eq!(batch.timeout_percent(), Some(25.0));
    }
}
