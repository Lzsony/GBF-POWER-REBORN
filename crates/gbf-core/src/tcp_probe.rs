//! TCP-only measurements. Resolve before timing; never exchange application data.
use crate::metrics::ProbeSample;
use std::{net::SocketAddr, time::Duration};
use tokio::{net::TcpStream, time::Instant};
use tokio_util::sync::CancellationToken;

pub(crate) async fn resolve(host: &str, port: u16) -> Vec<SocketAddr> {
    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::lookup_host((host, port)),
    )
    .await
    {
        Ok(Ok(addresses)) => addresses.take(16).collect(),
        _ => Vec::new(),
    }
}
pub(crate) async fn sample(addresses: &[SocketAddr]) -> ProbeSample {
    let deadline = Instant::now() + Duration::from_secs(2);
    for address in addresses {
        let started = Instant::now();
        match tokio::time::timeout_at(deadline, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => {
                let ms = started.elapsed().as_millis() as u64;
                drop(stream);
                return ProbeSample::Success(ms);
            }
            Err(_) => return ProbeSample::Timeout,
            Ok(Err(_)) => (),
        }
    }
    ProbeSample::Failure
}
pub(crate) async fn batch(
    host: &str,
    port: u16,
    cancel: &CancellationToken,
) -> crate::probe::Batch {
    let mut samples = Vec::new();
    let addresses = tokio::select! { _=cancel.cancelled()=>return crate::probe::Batch{samples}, a=resolve(host,port)=>a };
    for _ in 0..3 {
        tokio::select! { _=cancel.cancelled()=>break, value=sample(&addresses)=>samples.push(value) }
    }
    crate::probe::Batch { samples }
}
