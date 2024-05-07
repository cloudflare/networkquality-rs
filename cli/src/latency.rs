use std::sync::Arc;

use anyhow::Context;
use nq_core::{Network, StdTime, Time};
use nq_latency::{Latency, LatencyConfig, LatencyResult};
use nq_tokio_network::TokioNetwork;
use shellflip::{ShutdownCoordinator, ShutdownSignal};
use tracing::info;

use crate::util::pretty_secs_to_ms;

pub async fn run(url: String, runs: usize) -> anyhow::Result<()> {
    info!("measuring rtt with {runs} runs against {url}");

    if runs == 0 {
        anyhow::bail!("latency runs must be >= 1");
    }

    let result = run_test(&LatencyConfig {
        url: url.parse()?,
        runs,
    })
    .await?;

    let latency_ms = result
        .median()
        .map(pretty_secs_to_ms)
        .context("no measurements found, median rtt is null")?;

    let jitter_ms = result.jitter().map(pretty_secs_to_ms).unwrap_or(0.0);

    let json = serde_json::json!({
        "jitter_ms": jitter_ms,
        "latency_ms": latency_ms,
    });

    println!("{:#}", json);

    Ok(())
}

pub async fn run_test(config: &LatencyConfig) -> anyhow::Result<LatencyResult> {
    let shutdown_coordinator = ShutdownCoordinator::default();
    let time = Arc::new(StdTime) as Arc<dyn Time>;
    let network = Arc::new(TokioNetwork::new(
        Arc::clone(&time),
        shutdown_coordinator.handle(),
    )) as Arc<dyn Network>;

    let rtt = Latency::new(config.clone());
    let results = rtt.run_test(network, time, ShutdownSignal::from(&*shutdown_coordinator.handle())).await?;

    Ok(results)
}
