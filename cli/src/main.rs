pub(crate) mod args;
mod report;
mod rpm;
mod up_down;

use clap::Parser;
use nq_core::{Network, StdTime, Time};
use nq_tokio_network::TokioNetwork;
use tokio::sync::oneshot;

use std::sync::Arc;

use crate::args::rpm::RpmArgs;
use crate::args::Command;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = args::Cli::parse();

    // todo(fisher): control verbosity
    tracing_subscriber::fmt::init();

    let time = Arc::new(StdTime) as Arc<dyn Time>;
    let network = Arc::new(TokioNetwork::new(Arc::clone(&time))) as Arc<dyn Network>;

    let (_shutdown_tx, shutdown_rx) = oneshot::channel();

    // default to RPM
    let command = args
        .command
        .unwrap_or_else(|| Command::Rpm(RpmArgs::default()));

    match command {
        Command::Rpm(config) => rpm::run(config, network, time, shutdown_rx).await?,
        Command::Download(config) => up_down::download(config, network, time, shutdown_rx).await?,
        _ => unimplemented!(),
    }

    Ok(())
}
