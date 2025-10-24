// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Arguments for running a network saturation test.

use crate::args::ConnType;

/// Download data (GET) from an endpoint, reporting latency measurements and
/// total throughput.
#[derive(Debug, clap::Args)]
pub struct SaturateArgs {
    /// The connection type to use.
    #[clap(short = 't', long, default_value = "h1-clear-text")]
    pub(crate) conn_type: ConnType,
    /// Which direction to saturate: `up`, `down` or `both`.
    #[clap(subcommand)]
    pub(crate) direction: Direction,
    /// The duration in seconds to saturate the network for. Defaults
    /// to 20s.
    #[clap(long, default_value = "20")]
    pub(crate) duration: u64,
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum Direction {
    /// Saturate the download (ingress) side of the network.
    Down {
        /// The URL to upload data to.
        #[clap(
            short,
            long,
            default_value = "http://speed.cloudflare.com/__down?bytes=1000000000"
        )]
        download_url: String,
    },
    /// Saturate the upload (egress) side of the network.
    Up {
        /// The URL to upload data to.
        #[clap(short, long, default_value = "http://speed.cloudflare.com/__up")]
        upload_url: String,
    },
    /// Saturate both the download (ingress) and upload (egress) side of the network.
    Both {
        /// The URL to download data from.
        #[clap(
            short,
            long,
            default_value = "http://speed.cloudflare.com/__down?bytes=1000000000"
        )]
        download_url: String,
        /// The URL to upload data to.
        #[clap(short, long, default_value = "http://speed.cloudflare.com/__up")]
        upload_url: String,
    },
}

impl Default for Direction {
    fn default() -> Self {
        Self::Down {
            download_url: "http://speed.cloudflare.com/__down?bytes=1000000000".into(),
        }
    }
}

pub(crate) struct Urls {
    pub(crate) upload: String,
    pub(crate) download: String,
}

impl Direction {
    pub(crate) fn urls(&self) -> Urls {
        match self {
            Direction::Up { upload_url: url } | Direction::Down { download_url: url } => Urls {
                upload: url.clone(),
                download: url.clone(),
            },
            Direction::Both {
                download_url,
                upload_url,
            } => Urls {
                download: download_url.clone(),
                upload: upload_url.clone(),
            },
        }
    }
}
