// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

pub(crate) mod packet_loss;
pub(crate) mod rpm;
pub(crate) mod up_down;

use clap::{Parser, Subcommand, ValueEnum};
use nq_core::ConnectionType;
use packet_loss::PacketLossArgs;

use crate::args::rpm::RpmArgs;
use crate::args::up_down::DownloadArgs;

/// mach runs multiple different network performance tests. The main focus of
/// mach and this tool is to implement the IETF draft: "Responsiveness under
/// Working Conditions".
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub verbosity: clap_verbosity_flag::Verbosity,
    #[clap(subcommand)]
    pub command: Option<Command>,
    // todo(fisher): figure out proxies
    // #[clap(short = 'p', long = "proxy")]
    // proxies: Vec<Proxy>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Measure the network's responsiveness and report the download and upload
    /// capacity.
    ///
    /// This implements "Responsiveness under Working Conditions" draft:
    /// https://datatracker.ietf.org/doc/html/draft-ietf-ippm-responsiveness-03
    Rpm(RpmArgs),
    /// Download data (GET) from an endpoint, reporting latency measurements and total
    /// throughput.
    Download(DownloadArgs),
    /// Upload data (POST) to an endpoint,  reporting latency measurements and total
    /// throughput.
    // Upload(UploadArgs),
    /// Determine the Round-Trip-Time (RTT), or latency, of a link using the
    /// time it takes to establish a TCP connection.
    ///
    /// This is not a perfect measurement of RTT, but it's close.
    Rtt {
        /// The URL to perform a GET request against. The full GET time is not
        /// measured, just the TCP handshake.
        #[clap(default_value = "https://h3.speed.cloudflare.com/__down?bytes=10")]
        #[clap(short, long)]
        url: String,
        /// How many measurements to perform.
        #[clap(default_value = "20")]
        #[clap(short, long)]
        runs: usize,
    },
    /// Send UDP packets to a TURN server, reporting lost packets.
    PacketLoss(PacketLossArgs),
}

// todo(fisher): figure out proxy chaining. Preparsing args or using the -- sentinal?
// #[derive(Debug, Clone, Args)]
// struct Proxy {
//     /// The type of a proxy: h1, h2 or h3.
//     // #[clap(short = 't', long = "type")]
//     #[clap(short = 'h', long = "header")]
//     proxy_type: ProxyType,
//     /// The proxy's endpoint.
//     #[clap(short = 'h', long = "header")]
//     endpoint: String,
//     /// Headers sent on each connection to the proxy.
//     #[clap(short = 'h', long = "header")]
//     headers: Vec<String>,
// }

/// Describes which underlying transport a connection uses.
#[derive(Debug, Clone, ValueEnum)]
pub enum ConnType {
    H1ClearText,
    H1,
    H2,
    H3,
}

impl From<ConnType> for ConnectionType {
    fn from(conn_type: ConnType) -> Self {
        match conn_type {
            ConnType::H1ClearText => ConnectionType::H1 { use_tls: false },
            ConnType::H1 => ConnectionType::H1 { use_tls: true },
            ConnType::H2 => ConnectionType::H2,
            ConnType::H3 => ConnectionType::H3,
        }
    }
}
