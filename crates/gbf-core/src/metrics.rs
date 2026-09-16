use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

#[derive(Default)]
pub struct Metrics {
    pub requests: AtomicU64,
    pub downloads: AtomicU64,
    pub prefetched: AtomicU64,
    pub foreground: AtomicU64,
    tunnel_activity: Mutex<Option<tokio::time::Instant>>,
    pub eligible: AtomicU64,
    pub hits: AtomicU64,
    pub connections: AtomicU64,
    pub received: AtomicU64,
    pub sent: AtomicU64,
    network: Mutex<NetworkQuality>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProbeSample {
    Success(u64),
    Timeout,
    Failure,
}
impl ProbeSample {
    pub fn latency_ms(self) -> Option<u64> {
        if let Self::Success(ms) = self {
            Some(ms)
        } else {
            None
        }
    }
}

#[derive(Default)]
struct QualityWindow {
    samples: VecDeque<ProbeSample>,
    times: VecDeque<(tokio::time::Instant, std::time::SystemTime)>,
}
impl QualityWindow {
    fn record(&mut self, sample: ProbeSample) {
        if self.age().is_some_and(|age| age > 30.0) {
            self.samples.clear();
            self.times.clear();
        }
        if self.samples.len() == 20 {
            self.samples.pop_front();
            self.times.pop_front();
        }
        self.samples.push_back(sample);
        self.times
            .push_back((tokio::time::Instant::now(), std::time::SystemTime::now()));
        while self
            .times
            .front()
            .is_some_and(|time| sample_age(time) > 200.0)
        {
            self.times.pop_front();
            self.samples.pop_front();
        }
    }
    fn age(&self) -> Option<f64> {
        self.times.back().map(sample_age)
    }
    fn fresh(&self) -> bool {
        self.age().is_some_and(|age| age <= 30.0)
    }
    fn valid_samples(&self) -> impl Iterator<Item = &ProbeSample> {
        self.samples
            .iter()
            .zip(&self.times)
            .filter(|(_, time)| sample_age(time) <= 200.0)
            .map(|(sample, _)| sample)
    }
    fn distribution(&self) -> (Option<f64>, Option<u64>, Option<f64>, Option<u64>) {
        if !self.fresh() || self.samples.back().and_then(|s| s.latency_ms()).is_none() {
            return (None, None, None, None);
        }
        let mut values: Vec<_> = self
            .valid_samples()
            .filter_map(|s| s.latency_ms())
            .collect();
        if values.is_empty() {
            return (None, None, None, None);
        }
        values.sort_unstable();
        let n = values.len();
        let median = if n % 2 == 0 {
            (values[n / 2 - 1] as f64 + values[n / 2] as f64) / 2.0
        } else {
            values[n / 2] as f64
        };
        (
            Some(values.iter().map(|x| *x as f64).sum::<f64>() / n as f64),
            values.first().copied(),
            Some(median),
            values.last().copied(),
        )
    }
    fn snapshot(&self) -> (Option<u64>, Option<f64>, Option<f64>) {
        if !self.fresh() {
            return (None, None, None);
        }
        let successful: Vec<_> = self
            .valid_samples()
            .filter_map(|sample| sample.latency_ms())
            .collect();
        let differences: Vec<_> = successful
            .windows(2)
            .map(|pair| pair[0].abs_diff(pair[1]))
            .collect();
        let jitter = (!differences.is_empty()
            && self.samples.back().and_then(|s| s.latency_ms()).is_some())
        .then(|| {
            differences.iter().map(|value| *value as f64).sum::<f64>() / differences.len() as f64
        });
        let timeouts = self
            .valid_samples()
            .filter(|sample| matches!(sample, ProbeSample::Timeout))
            .count();
        let rate = (!self.samples.is_empty())
            .then(|| timeouts as f64 * 100.0 / self.valid_samples().count() as f64);
        (
            self.samples.back().and_then(|sample| sample.latency_ms()),
            jitter,
            rate,
        )
    }
}
fn sample_age(time: &(tokio::time::Instant, std::time::SystemTime)) -> f64 {
    let monotonic = time.0.elapsed().as_secs_f64();
    let wall = std::time::SystemTime::now()
        .duration_since(time.1)
        .map(|d| d.as_secs_f64())
        .unwrap_or(f64::INFINITY);
    monotonic.max(wall)
}
#[derive(Default)]
struct NetworkQuality {
    line: QualityWindow,
    game: QualityWindow,
    steam: QualityWindow,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSnapshot {
    pub line_age_secs: Option<f64>,
    pub game_age_secs: Option<f64>,
    pub steam_age_secs: Option<f64>,
    pub line_latency_ms: Option<u64>,
    pub line_mean_ms: Option<f64>,
    pub line_jitter_ms: Option<f64>,
    pub line_timeout_percent: Option<f64>,
    pub game_latency_ms: Option<u64>,
    pub game_min_ms: Option<u64>,
    pub game_median_ms: Option<f64>,
    pub game_max_ms: Option<u64>,
    pub game_jitter_ms: Option<f64>,
    pub game_timeout_percent: Option<f64>,
    pub steam_latency_ms: Option<u64>,
    pub steam_min_ms: Option<u64>,
    pub steam_median_ms: Option<f64>,
    pub steam_max_ms: Option<u64>,
    pub steam_jitter_ms: Option<f64>,
    pub steam_timeout_percent: Option<f64>,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub requests: u64,
    pub downloads: u64,
    pub prefetched: u64,
    pub hit_rate: Option<f64>,
    pub received: u64,
    pub sent: u64,
    pub network: NetworkSnapshot,
    pub uptime_secs: u64,
}
impl Metrics {
    pub fn record_probe(&self, target: usize, sample: ProbeSample) {
        let mut network = self.network.lock().unwrap();
        match target {
            0 => &mut network.line,
            1 => &mut network.game,
            _ => &mut network.steam,
        }
        .record(sample);
    }
    pub fn reset_probe(&self, target: usize) {
        let mut network = self.network.lock().unwrap();
        *match target {
            0 => &mut network.line,
            1 => &mut network.game,
            _ => &mut network.steam,
        } = QualityWindow::default();
    }
    pub fn tunnel_activity(&self) {
        *self.tunnel_activity.lock().unwrap() = Some(tokio::time::Instant::now());
    }
    pub fn foreground_busy(&self) -> bool {
        self.foreground.load(Ordering::Relaxed) > 0
            || self
                .tunnel_activity
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed().as_millis() < 500)
    }
    pub fn foreground(self: &Arc<Self>) -> ForegroundGuard {
        self.foreground.fetch_add(1, Ordering::Relaxed);
        ForegroundGuard(self.clone())
    }
    pub fn record_network_sample(
        &self,
        line: Option<ProbeSample>,
        game: ProbeSample,
        steam: ProbeSample,
    ) {
        let mut network = self.network.lock().unwrap();
        if let Some(sample) = line {
            network.line.record(sample);
        }
        network.game.record(game);
        network.steam.record(steam);
    }

    pub fn reset_line(&self) {
        self.network.lock().unwrap().line = QualityWindow::default();
    }
    fn network_snapshot(&self) -> NetworkSnapshot {
        let network = self.network.lock().unwrap();
        let (line_latency_ms, line_jitter_ms, line_timeout_percent) = network.line.snapshot();
        let (game_latency_ms, game_jitter_ms, game_timeout_percent) = network.game.snapshot();
        let (steam_latency_ms, steam_jitter_ms, steam_timeout_percent) = network.steam.snapshot();
        let (line_mean_ms, _, _, _) = network.line.distribution();
        let (_, game_min_ms, game_median_ms, game_max_ms) = network.game.distribution();
        let (_, steam_min_ms, steam_median_ms, steam_max_ms) = network.steam.distribution();
        NetworkSnapshot {
            line_age_secs: network.line.age().filter(|age| age.is_finite()),
            game_age_secs: network.game.age().filter(|age| age.is_finite()),
            steam_age_secs: network.steam.age().filter(|age| age.is_finite()),
            line_mean_ms,
            game_min_ms,
            game_median_ms,
            game_max_ms,
            steam_min_ms,
            steam_median_ms,
            steam_max_ms,
            line_latency_ms,
            line_jitter_ms,
            line_timeout_percent,
            game_latency_ms,
            game_jitter_ms,
            game_timeout_percent,
            steam_latency_ms,
            steam_jitter_ms,
            steam_timeout_percent,
        }
    }

    pub fn snapshot(&self, started: Option<Instant>) -> Snapshot {
        let n = |v: &AtomicU64| v.load(Ordering::Relaxed);
        Snapshot {
            requests: n(&self.requests),
            downloads: n(&self.downloads),
            prefetched: n(&self.prefetched),
            hit_rate: (n(&self.eligible) > 0)
                .then(|| n(&self.hits) as f64 * 100.0 / n(&self.eligible) as f64),
            received: n(&self.received),
            sent: n(&self.sent),
            network: self.network_snapshot(),
            uptime_secs: started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
        }
    }
    pub fn connection(self: &Arc<Self>) -> ConnectionGuard {
        self.connections.fetch_add(1, Ordering::Relaxed);
        ConnectionGuard(self.clone())
    }
}

pub struct ConnectionGuard(Arc<Metrics>);
pub struct ForegroundGuard(Arc<Metrics>);
impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        self.0.tunnel_activity();
        self.0.foreground.fetch_sub(1, Ordering::Relaxed);
    }
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProbeSample::{Failure, Success, Timeout};
    #[tokio::test(start_paused = true)]
    async fn windows_expire_and_long_gaps_start_fresh() {
        let mut window = QualityWindow::default();
        window.record(Success(10));
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        assert_eq!(window.distribution().2, Some(10.0));
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert_eq!(window.distribution().2, None);
        assert_eq!(window.snapshot().2, None);
        window.record(Success(80));
        assert_eq!(window.distribution().2, Some(80.0));
        for _ in 0..12 {
            tokio::time::advance(std::time::Duration::from_secs(20)).await;
            window.record(Success(20));
        }
        assert!(window.samples.len() <= 11);
        assert_eq!(window.distribution().2, Some(20.0));
        window.record(Failure);
        assert_eq!(window.distribution().2, None);
        assert_eq!(window.snapshot().2, Some(0.0));
    }

    #[tokio::test(start_paused = true)]
    async fn cooldown_tracks_foreground_completion_and_api_activity() {
        let metrics = Arc::new(Metrics::default());
        assert!(!metrics.foreground_busy());
        let guard = metrics.foreground();
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert!(metrics.foreground_busy());
        drop(guard);
        tokio::time::advance(std::time::Duration::from_millis(499)).await;
        assert!(metrics.foreground_busy());
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert!(!metrics.foreground_busy());
        metrics.tunnel_activity();
        assert!(metrics.foreground_busy());
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        assert!(!metrics.foreground_busy());
    }
    #[test]
    fn distributions_ignore_failed_samples_and_reset_only_route() {
        let mut w = QualityWindow::default();
        assert_eq!(w.distribution(), (None, None, None, None));
        w.record(ProbeSample::Success(10));
        assert_eq!(
            w.distribution(),
            (Some(10.0), Some(10), Some(10.0), Some(10))
        );
        assert_eq!(w.snapshot().1, None);
        w.record(ProbeSample::Timeout);
        w.record(ProbeSample::Failure);
        w.record(ProbeSample::Success(21));
        assert_eq!(
            w.distribution(),
            (Some(15.5), Some(10), Some(15.5), Some(21))
        );
        assert_eq!(w.snapshot().2, Some(25.0));
        w.record(ProbeSample::Success(100));
        assert_eq!(w.distribution().2, Some(21.0));
        let metrics = Metrics::default();
        metrics.record_network_sample(
            Some(ProbeSample::Success(10)),
            ProbeSample::Success(20),
            ProbeSample::Success(30),
        );
        metrics.reset_line();
        let n = metrics.network_snapshot();
        assert_eq!(n.line_mean_ms, None);
        assert_eq!(n.game_min_ms, Some(20));
        assert_eq!(n.steam_median_ms, Some(30.0));
    }
    #[test]
    fn quality_windows_count_timeouts_separately_and_evict_old_attempts() {
        let metrics = Metrics::default();
        for sample in [Success(10), Success(20), Timeout, Failure, Success(40)] {
            metrics.record_network_sample(Some(sample), Success(8), Failure);
        }
        let snapshot = metrics.network_snapshot();
        assert_eq!(snapshot.line_latency_ms, Some(40));
        assert_eq!(snapshot.line_jitter_ms, Some(15.0));
        assert_eq!(snapshot.line_timeout_percent, Some(20.0));
        assert_eq!(snapshot.game_latency_ms, Some(8));
        assert_eq!(snapshot.game_jitter_ms, Some(0.0));
        assert_eq!(snapshot.game_timeout_percent, Some(0.0));
        assert_eq!(snapshot.steam_latency_ms, None);
        assert_eq!(snapshot.steam_timeout_percent, Some(0.0));
        for value in 0..20 {
            metrics.record_network_sample(Some(Success(value)), Timeout, Success(value * 2));
        }
        let snapshot = metrics.network_snapshot();
        assert_eq!(snapshot.line_latency_ms, Some(19));
        assert_eq!(snapshot.line_jitter_ms, Some(1.0));
        assert_eq!(snapshot.line_timeout_percent, Some(0.0));
        assert_eq!(snapshot.game_timeout_percent, Some(100.0));
        assert_eq!(snapshot.steam_jitter_ms, Some(2.0));
        assert_eq!(snapshot.steam_timeout_percent, Some(0.0));
    }

    #[test]
    fn missing_samples_and_failed_latest_latency_are_not_zero_latency() {
        let metrics = Metrics::default();
        let snapshot = metrics.network_snapshot();
        assert_eq!(snapshot.line_timeout_percent, None);
        assert_eq!(snapshot.game_timeout_percent, None);
        metrics.record_network_sample(None, Success(10), Timeout);
        let snapshot = metrics.network_snapshot();
        assert_eq!(snapshot.line_timeout_percent, None);
        assert_eq!(snapshot.game_jitter_ms, None);
        assert_eq!(snapshot.steam_timeout_percent, Some(100.0));
        metrics.record_network_sample(Some(Success(20)), Success(20), Success(15));
        metrics.record_network_sample(Some(Failure), Failure, Success(15));
        let snapshot = metrics.network_snapshot();
        assert_eq!(snapshot.line_latency_ms, None);
        assert_eq!(snapshot.line_timeout_percent, Some(0.0));
        assert_eq!(snapshot.game_latency_ms, None);
        assert_eq!(snapshot.game_jitter_ms, None);
        assert_eq!(snapshot.game_median_ms, None);
    }
}
