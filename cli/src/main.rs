use nq_speedtests::{Responsiveness, ResponsivenessConfig, Speedtest};
// use nq_core::{Responsiveness, ResponsivenessConfig, Speedtest};
use nq_io::OSNetwork;
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let network = OSNetwork::default();

    let (_, shutdown_rx) = oneshot::channel();

    Responsiveness::new(ResponsivenessConfig::default())?
        .run(network, shutdown_rx)
        .await?;

    Ok(())
}
