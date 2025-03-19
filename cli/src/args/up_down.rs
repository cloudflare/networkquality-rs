// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Arguments for running simple upload and download tests.

use clap::Args;

use crate::args::ConnType;

/// Download data (GET) from an endpoint, reporting latency measurements and
/// total throughput.
#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// The URL to download data from.
    #[clap(default_value = "https://h3.speed.cloudflare.com/__down?bytes=10")]
    pub(crate) url: String,
    #[clap(default_value = "h2")]
    pub(crate) conn_type: ConnType,
    #[clap(short = 'H', long = "header")]
    pub(crate) headers: Vec<String>,
}

/// Upload data (POST) to an endpoint,  reporting latency measurements and total
/// throughput.
#[derive(Debug, Args)]
pub struct UploadArgs {
    /// The URL to upload data to.
    #[clap(default_value = "https://h3.speed.cloudflare.com/__up")]
    pub(crate) url: String,
    /// The type of the connection.
    #[clap(default_value = "h2")]
    pub(crate) conn_type: ConnType,
    /// The number of arbitrary bytes to upload. Only one of `bytes` or `file`
    /// can be set.
    #[clap(short, long)]
    pub(crate) bytes: Option<usize>,
    /// Upload the contents of a file. Only one of `bytes` or `file` can be set.
    // #[clap(short, long)]
    // pub(crate) file: Option<PathBuf>,
    /// Headers to add to the request.
    #[clap(short = 'H', long = "header")]
    pub(crate) headers: Vec<String>,
}
