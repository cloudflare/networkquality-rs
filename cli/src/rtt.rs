use std::sync::Arc;

use anyhow::Context;
use nq_core::{Network, Speedtest, StdTime, Time};
use nq_rtt::{Rtt, RttConfig, RttResult};
use nq_tokio_network::TokioNetwork;
use shellflip::{ShutdownCoordinator, ShutdownSignal};

use crate::util::pretty_secs_to_ms;

pub async fn run(url: String, runs: usize) -> anyhow::Result<()> {
    let result = run_test(&RttConfig {
        url: url.parse()?,
        runs,
    })
    .await?;

    let rtt_ms = pretty_secs_to_ms(
        result
            .measurements
            .quantile(0.50)
            .context("no measurements found, median rtt is null")?,
    );

    let json = serde_json::json!({
        "rtt_ms": rtt_ms,
    });

    println!("{}", json);

    Ok(())
}

pub async fn run_test(config: &RttConfig) -> anyhow::Result<RttResult> {
    let shutdown_coordinator = ShutdownCoordinator::default();
    let time = Arc::new(StdTime) as Arc<dyn Time>;
    let network = Arc::new(TokioNetwork::new(
        Arc::clone(&time),
        shutdown_coordinator.handle(),
    )) as Arc<dyn Network>;

    let rtt = Rtt::new(config.clone());

    let results = rtt
        .run(
            network,
            time,
            ShutdownSignal::from(&*shutdown_coordinator.handle()),
        )
        .await?;

    Ok(results)
}
