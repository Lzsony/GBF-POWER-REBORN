//! One-time selection. The selected tunnel is retained; game requests are never replayed.
use crate::{
    acceleration::{Line, SharedStatus, Tunnel},
    config::Settings,
    error::ErrorCode,
    routing,
};
use anyhow::Result;
use futures_util::{stream, StreamExt};
use std::{
    future::Future,
    path::Path,
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score {
    pub failures: usize,
    pub median_micros: u128,
    pub id: String,
}
impl Score {
    pub fn from_samples(id: String, samples: &[Option<u128>]) -> Option<Self> {
        let mut good: Vec<_> = samples.iter().flatten().copied().collect();
        if good.is_empty() {
            return None;
        }
        good.sort_unstable();
        let mid = good.len() / 2;
        let median = if good.len() % 2 == 0 {
            (good[mid - 1] + good[mid]) / 2
        } else {
            good[mid]
        };
        Some(Self {
            failures: samples.len() - good.len(),
            median_micros: median,
            id,
        })
    }
}
pub struct Selected {
    pub tunnel: Tunnel,
    pub score: Score,
}

pub async fn select(root: &Path, settings: &Settings, lines: Vec<Line>) -> Result<Selected> {
    select_candidates(lines, |line| async move {
        let status: SharedStatus = Arc::new(RwLock::new(Default::default()));
        let id = line.id.clone();
        let mut tunnel = match Tunnel::start(root, line, status).await {
            Ok(t) => t,
            Err(_) => return None,
        };
        let client = match routing::client(&tunnel.routed_settings(settings), "", true) {
            Ok(c) => c,
            Err(_) => {
                tunnel.stop().await;
                return None;
            }
        };
        let batch = crate::probe::batch(&client, crate::probe::GAME).await;
        let samples: Vec<_> = batch
            .samples
            .iter()
            .map(|s| s.latency_ms().map(|ms| u128::from(ms) * 1000))
            .collect();
        match Score::from_samples(id, &samples) {
            Some(score) => Some(Selected { tunnel, score }),
            None => {
                tunnel.stop().await;
                None
            }
        }
    })
    .await
}
async fn select_candidates<F, Fut>(lines: Vec<Line>, probe: F) -> Result<Selected>
where
    F: FnMut(Line) -> Fut,
    Fut: Future<Output = Option<Selected>>,
{
    let candidates = stream::iter(lines.into_iter().map(probe)).buffer_unordered(2);
    tokio::pin!(candidates);
    let mut best: Option<Selected> = None;
    while let Some(candidate) = candidates.next().await {
        if let Some(mut candidate) = candidate {
            if best.as_ref().is_none_or(|b| candidate.score < b.score) {
                if let Some(mut previous) = best.take() {
                    previous.tunnel.stop().await;
                }
                best = Some(candidate);
            } else {
                candidate.tunnel.stop().await;
            }
        }
    }
    best.ok_or_else(|| ErrorCode::SshConnectionFailed.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn ranks_failures_before_median_and_stably_breaks_ties() {
        let fast_unreliable =
            Score::from_samples("fast".into(), &[Some(1), None, Some(1)]).unwrap();
        let good = Score::from_samples("a".into(), &[Some(30), Some(10), Some(20)]).unwrap();
        assert!(good < fast_unreliable);
        assert_eq!(good.median_micros, 20);
        assert!(good < Score::from_samples("b".into(), &[Some(10), Some(20), Some(30)]).unwrap());
        assert!(Score::from_samples("bad".into(), &[None, None, None]).is_none());
        assert_eq!(
            Score::from_samples("two".into(), &[Some(10), None, Some(30)])
                .unwrap()
                .median_micros,
            20
        );
    }
    fn alive(root: &Path) -> bool {
        let Ok(pid) = std::fs::read_to_string(root.join("ssh/pid")) else {
            return false;
        };
        crate::acceleration::tests::process_alive(&pid)
    }
    #[tokio::test]
    async fn selection_limits_parallelism_retains_winner_and_reaps_losers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixtures: Vec<_> = (0..4)
            .map(|i| crate::acceleration::tests::fixture(if i == 3 { "auth" } else { "normal" }))
            .collect();
        let lines = fixtures
            .iter()
            .enumerate()
            .map(|(i, (_, line, _, _))| {
                let mut l = line.clone();
                l.id = i.to_string();
                l
            })
            .collect();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let mut selected = select_candidates(lines, |line| {
            let (dir, _, exe, status) = &fixtures[line.id.parse::<usize>().unwrap()];
            let active = &active;
            let peak = &peak;
            async move {
                let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(n, Ordering::SeqCst);
                let result = Tunnel::start_with(
                    dir.path(),
                    line.clone(),
                    status.clone(),
                    exe.clone(),
                    "http://game.granbluefantasy.jp/".into(),
                )
                .await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                result.ok().map(|tunnel| Selected {
                    tunnel,
                    score: Score::from_samples(line.id, &[Some(10), Some(10), Some(10)]).unwrap(),
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(selected.score.id, "0");
        assert!(alive(fixtures[0].0.path()));
        for f in fixtures.iter().skip(1) {
            assert!(!alive(f.0.path()));
        }
        let port = selected.tunnel.port;
        selected.tunnel.stop().await;
        assert!(!alive(fixtures[0].0.path()));
        assert!(
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_ok()
        );
        assert!(select_candidates(Vec::new(), |_| async { None })
            .await
            .is_err());
    }
    #[tokio::test]
    async fn cancelling_selection_reaps_inflight_tunnels() {
        let a = crate::acceleration::tests::fixture("normal");
        let b = crate::acceleration::tests::fixture("normal");
        let roots = [a.0.path().to_path_buf(), b.0.path().to_path_buf()];
        let fixtures = Arc::new(vec![a, b]);
        let keep = fixtures.clone();
        let task = tokio::spawn(async move {
            let lines = fixtures
                .iter()
                .enumerate()
                .map(|(i, (_, line, _, _))| {
                    let mut l = line.clone();
                    l.id = i.to_string();
                    l
                })
                .collect();
            select_candidates(lines, |line| {
                let f = fixtures.clone();
                async move {
                    let (dir, _, exe, status) = &f[line.id.parse::<usize>().unwrap()];
                    let tunnel = Tunnel::start_with(
                        dir.path(),
                        line,
                        status.clone(),
                        exe.clone(),
                        "http://game.granbluefantasy.jp/".into(),
                    )
                    .await
                    .unwrap();
                    std::future::pending::<()>().await;
                    drop(tunnel);
                    None
                }
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !roots.iter().all(|r| alive(r)) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while roots.iter().any(|r| alive(r)) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        drop(keep);
    }
}
