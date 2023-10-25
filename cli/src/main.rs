use nq_core::{Network, Speedtest, StdTime, Time};
use nq_rpm::{Responsiveness, ResponsivenessConfig};
use nq_tokio_network::TokioNetwork;
use tokio::sync::oneshot;

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let time = Arc::new(StdTime) as Arc<dyn Time>;
    let network = Arc::new(TokioNetwork::new(Arc::clone(&time))) as Arc<dyn Network>;

    let (_shutdown_tx, shutdown_rx) = oneshot::channel();

    let config = ResponsivenessConfig::default();
    let rpm = Responsiveness::new(config)?;

    let test_result = rpm.run(network, time, shutdown_rx).await?;

    println!("{:?}", test_result);

    Ok(())
}
