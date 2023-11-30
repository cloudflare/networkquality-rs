mod aim_report;
pub(crate) mod args;
mod report;
mod rpm;
mod rtt;
mod up_down;
mod util;

use clap::Parser;

use crate::args::rpm::RpmArgs;
use crate::args::Command;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = args::Cli::parse();

    // todo(fisher): control verbosity
    tracing_subscriber::fmt::init();

    // default to RPM
    let command = args
        .command
        .unwrap_or_else(|| Command::Rpm(RpmArgs::default()));

    match command {
        Command::Rpm(config) => rpm::run(config).await?,
        Command::Download(config) => up_down::download(config).await?,
        Command::Upload(config) => up_down::upload(config).await?,
        Command::Rtt { url, runs } => rtt::run(url, runs).await?,
    }

    Ok(())
}
