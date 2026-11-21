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
        /// The URL to download data from. Defaults to
        /// `h3.speed.cloudflare.com/__down`, over `http://` for
        /// `h1-clear-text` and `https://` otherwise.
        #[clap(short, long)]
        download_url: Option<String>,
    },
    /// Saturate the upload (egress) side of the network.
    Up {
        /// The URL to upload data to. Defaults to
        /// `h3.speed.cloudflare.com/__up`, over `http://` for `h1-clear-text`
        /// and `https://` otherwise.
        #[clap(short, long)]
        upload_url: Option<String>,
    },
    /// Saturate both the download (ingress) and upload (egress) side of the network.
    Both {
        /// The URL to download data from. Defaults to
        /// `h3.speed.cloudflare.com/__down`, over `http://` for
        /// `h1-clear-text` and `https://` otherwise.
        #[clap(short, long)]
        download_url: Option<String>,
        /// The URL to upload data to. Defaults to
        /// `h3.speed.cloudflare.com/__up`, over `http://` for `h1-clear-text`
        /// and `https://` otherwise.
        #[clap(short, long)]
        upload_url: Option<String>,
    },
}

impl Default for Direction {
    fn default() -> Self {
        Self::Down { download_url: None }
    }
}

pub(crate) struct Urls {
    pub(crate) upload: String,
    pub(crate) download: String,
}

impl Direction {
    /// The URLs to use, with the default URLs matching the scheme of `conn_type`.
    pub(crate) fn urls(&self, conn_type: &ConnType) -> Urls {
        let scheme = match conn_type {
            ConnType::H1ClearText => "http",
            _ => "https",
        };
        let default_download =
            || format!("{scheme}://h3.speed.cloudflare.com/__down?bytes=10000000000");
        let default_upload = || format!("{scheme}://h3.speed.cloudflare.com/__up");

        match self {
            Direction::Up { upload_url } => {
                let url = upload_url.clone().unwrap_or_else(default_upload);
                Urls {
                    upload: url.clone(),
                    download: url,
                }
            }
            Direction::Down { download_url } => {
                let url = download_url.clone().unwrap_or_else(default_download);
                Urls {
                    upload: url.clone(),
                    download: url,
                }
            }
            Direction::Both {
                download_url,
                upload_url,
            } => Urls {
                download: download_url.clone().unwrap_or_else(default_download),
                upload: upload_url.clone().unwrap_or_else(default_upload),
            },
        }
    }
}
