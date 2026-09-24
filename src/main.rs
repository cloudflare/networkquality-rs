// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

mod access;
mod aim_report;
pub(crate) mod args;
mod latency;
mod nq_core;
mod nq_latency;
mod nq_load_generator;
mod nq_packetloss;
mod nq_rpm;
mod nq_stats;
mod nq_tokio_network;
mod packet_loss;
mod report;
mod rpm;
mod saturate;
mod up_down;
mod util;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use clap_verbosity_flag::LevelFilter;

use crate::args::Command;
use crate::args::rpm::RpmArgs;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = args::Cli::parse();

    if args.license {
        if args.command.is_some() {
            args::Cli::command()
                .bin_name("mach")
                .error(
                    ErrorKind::ArgumentConflict,
                    "the argument '--license' cannot be used with a subcommand",
                )
                .exit();
        }
        print!("{}", include_str!("../LICENSE"));
        return Ok(());
    }

    setup_logging(args.verbosity)?;

    // default to RPM
    let command = args
        .command
        .unwrap_or_else(|| Command::Rpm(RpmArgs::default()));

    match command {
        Command::Rpm(config) => rpm::run(config).await?,
        Command::Download(config) => up_down::download(config).await?,
        Command::Upload(config) => up_down::upload(config).await?,
        Command::Rtt { url, runs } => latency::run(url, runs).await?,
        Command::PacketLoss(config) => packet_loss::run(config).await?,
        Command::Saturate(config) => saturate::run(config).await?,
    }

    Ok(())
}

fn setup_logging(verbosity: clap_verbosity_flag::Verbosity) -> anyhow::Result<()> {
    let filter = if let Ok(log) = std::env::var("RUST_LOG") {
        log
    } else {
        match verbosity.log_level_filter() {
            LevelFilter::Off => "off",
            LevelFilter::Error => "error",
            LevelFilter::Warn => "warn",
            LevelFilter::Info => "info",
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
