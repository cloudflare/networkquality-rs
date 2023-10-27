pub(crate) mod rpm;
pub(crate) mod up_down;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::args::rpm::RpmArgs;
use crate::args::up_down::DownloadArgs;

///
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// Use verbose output
    #[clap(short, long)]
    pub verbose: bool,
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
    Upload {
        /// The URL to upload data to.
        #[clap(default_value = "https://aim.cloudflare.com/responsiveness/api/v1/upload")]
        url: String,
        /// The number of arbitrary bytes to upload. Only one of `bytes` or
        /// `file` can be set.
        #[clap(short, long)]
        bytes: Option<usize>,
        /// Upload the contents of a file. Only one of `bytes` or `file` can be
        /// set.
        #[clap(short, long)]
        file: Option<PathBuf>,
        /// Headers to add to the request.
        #[clap(short, long = "header")]
        headers: Vec<String>,
    },
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
    H1,
    H2,
    H3,
}
