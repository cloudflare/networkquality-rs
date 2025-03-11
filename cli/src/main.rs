// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

mod aim_report;
pub(crate) mod args;
mod latency;
mod packet_loss;
mod report;
mod rpm;
mod up_down;
mod util;

use clap::Parser;
use clap_verbosity_flag::LevelFilter;

use crate::args::rpm::RpmArgs;
use crate::args::Command;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = args::Cli::parse();

    setup_logging(args.verbosity)?;

    // default to RPM
    let command = args
        .command
        .unwrap_or_else(|| Command::Rpm(RpmArgs::default()));

    match command {
        Command::Rpm(config) => rpm::run(config).await?,
        Command::Download(config) => up_down::download(config).await?,
        // Command::Upload(config) => up_down::upload(config).await?,
        Command::Rtt { url, runs } => latency::run(url, runs).await?,
        Command::PacketLoss(config) => packet_loss::run(config).await?,
    }

    Ok(())
}

fn setup_logging(verbosity: clap_verbosity_flag::Verbosity) -> anyhow::Result<()> {
    let filter = if let Ok(log) = std::env::var("RUST_LOG") {
        log
    } else {
        match verbosity.log_level_filter() {
            LevelFilter::Off => "error",
            LevelFilter::Error => "mach=info,error",
            LevelFilter::Warn => "mach=info,nq_rpm=info,nq_latency=info,nq_core=error",
            LevelFilter::Info => "mach=info,nq_rpm=info,nq_latency=info,nq_core=info",
            LevelFilter::Debug => "debug",
            LevelFilter::Trace => "trace",
        }
        .to_string()
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    Ok(())
}
