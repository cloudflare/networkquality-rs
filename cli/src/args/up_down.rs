//! Arguments for running simple upload and download tests.

use clap::Args;

use crate::args::ConnType;

/// Download data (GET) from an endpoint, reporting latency measurements and
/// total throughput.
#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// The URL to download data from.
    #[clap(default_value = "https://aim.cloudflare.com/responsiveness/api/v1/small")]
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
    #[clap(default_value = "https://aim.cloudflare.com/responsiveness/api/v1/upload")]
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
