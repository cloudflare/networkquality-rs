// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Arguments for running responsiveness tests.

use clap::{Args, ValueEnum};
use nq_rpm::{ConnectionErrorPolicy, DEFAULT_UPLOAD_BYTES_PER_REQUEST};

/// Smallest accepted `--upload-max-request-bytes`.
///
/// Below roughly this size a request completes almost immediately, so the load
/// generator spends its time opening streams instead of moving bytes. That
/// generates very little actual load while hammering the server with requests,
/// which is both a useless measurement and unfriendly to the endpoint.
pub const MIN_UPLOAD_BYTES_PER_REQUEST: usize = 1024 * 1024;

/// Below this, warn that request overhead is becoming significant.
pub const SMALL_UPLOAD_BYTES_PER_REQUEST: usize = 16 * 1024 * 1024;

fn parse_upload_bytes_per_request(raw: &str) -> Result<usize, String> {
    let bytes: usize = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a whole number of bytes"))?;

    if bytes < MIN_UPLOAD_BYTES_PER_REQUEST {
        return Err(format!(
            "{bytes} is too small; the minimum is {MIN_UPLOAD_BYTES_PER_REQUEST} (1 MiB). \
             Requests this small complete instantly, so the load generator would spend the \
             test opening streams rather than saturating the link"
        ));
    }

    Ok(bytes)
}

/// CLI spelling of [`ConnectionErrorPolicy`].
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConnectionErrorPolicyArg {
    /// Retire the failed connection and keep measuring.
    Retire,
    /// Abort the test on the first failed connection.
    Abort,
}

impl From<ConnectionErrorPolicyArg> for ConnectionErrorPolicy {
    fn from(arg: ConnectionErrorPolicyArg) -> Self {
        match arg {
            ConnectionErrorPolicyArg::Retire => ConnectionErrorPolicy::Retire,
            ConnectionErrorPolicyArg::Abort => ConnectionErrorPolicy::Abort,
        }
    }
}

#[derive(Debug, Args)]
pub struct RpmArgs {
    /// The endpoint to get the responsiveness config from. Should be JSON in
    /// the form:
    ///
    /// {
    ///     "version": number,
    ///     "test_endpoint": string?,
    ///     "urls": {
    ///         "small_https_download_url": string,
    ///         "large_https_download_url": string,
    ///         "https_upload_url": string
    ///     }
    /// }
    #[clap(short = 'c', long = "config")]
    pub config: Option<String>,
    /// The large file endpoint which should be multiple GBs.
    #[clap(
        short = 'l',
        long = "large",
        default_value = "https://h3.speed.cloudflare.com/__down?bytes=10000000000"
    )]
    pub large_download_url: String,
    /// The small file endpoint which should be very small, only a few bytes.
    #[clap(
        short = 's',
        long = "small",
        default_value = "https://h3.speed.cloudflare.com/__down?bytes=10"
    )]
    pub small_download_url: String,
    /// The upload url which accepts an arbitrary amount of data.
    #[clap(
        short = 'u',
        long = "upload",
        default_value = "https://h3.speed.cloudflare.com/__up"
    )]
    pub upload_url: String,
    /// Skip TLS certificate verification. Only for testing against servers with
    /// self-signed certificates (e.g. a local `wrangler dev`). Never use against
    /// production endpoints.
    #[clap(long = "insecure", default_value = "false")]
    pub insecure: bool,
    /// What to do when a load-generating connection terminates with an error
    /// (for example an upload rejected with HTTP 413).
    ///
    /// `retire` drops the failed connection, lets the ramp replace it, and
    /// reports how many failed. `abort` stops the test on the first failure,
    /// which is what draft-ietf-ippm-responsiveness-09 §5.4 literally describes.
    #[clap(long = "on-connection-error", default_value = "retire")]
    pub on_connection_error: ConnectionErrorPolicyArg,
    /// The number of intervals to use when calculating the moving average.
    #[clap(long = "mad", default_value = "4")]
    pub moving_average_distance: usize,
    /// How far a measurement is allowed to be from the previous moving average
    /// before the measurement is considered unstable.
    #[clap(long = "std", default_value = "0.05")]
    pub std_tolerance: f64,
    /// Determines which percentile to use for averaging when calculating the
    /// trimmed mean of throughputs or RPM scores. A value of `0.95` means to
    /// only use values in the 95th percentile to calculate an average.
    #[clap(long = "trim", default_value = "0.95")]
    pub trimmed_mean_percent: f64,
    /// The maximum number of loaded connections that the test can use to
    /// saturate the network.
    #[clap(long = "max-load", default_value = "16")]
    pub max_loaded_connections: usize,
    /// Maximum bytes sent in any single upload request.
    ///
    /// Upload load is generated as a sequence of requests of this size on each
    /// connection, re-issued as they complete, rather than one enormous request.
    /// Servers may cap request body size and reject anything larger with HTTP
    /// 413. Such caps are per-request, so staying under one keeps the link
    /// loaded indefinitely.
    ///
    /// Lower this if uploads are being rejected. It has no effect on links too
    /// slow to send this much within the test duration.
    #[clap(
        long = "upload-max-request-bytes",
        default_value_t = DEFAULT_UPLOAD_BYTES_PER_REQUEST,
        value_parser = parse_upload_bytes_per_request,
    )]
    pub upload_bytes_per_request: usize,
    /// The duration between interval updates in milliseconds (ms).
    #[clap(long = "interval-duration", default_value = "1000")]
    pub interval_duration_ms: u64,
    /// The overall test duration in milliseconds (ms).
    #[clap(long = "test-duration", default_value = "12000")]
    pub test_duration_ms: u64,
    /// Disable AIM score reporting.
    ///
    /// https://blog.cloudflare.com/aim-database-for-internet-quality/
    #[clap(long)]
    pub disable_aim_scores: bool,
}

impl Default for RpmArgs {
    fn default() -> Self {
        Self {
            config: None,
            large_download_url: "https://h3.speed.cloudflare.com/__down?bytes=10000000000"
                .to_string(),
            small_download_url: "https://h3.speed.cloudflare.com/__down?bytes=10".to_string(),
            upload_url: "https://h3.speed.cloudflare.com/__up".to_string(),
            insecure: false,
            on_connection_error: ConnectionErrorPolicyArg::Retire,
            moving_average_distance: 4,
            std_tolerance: 0.05,
            trimmed_mean_percent: 0.95,
            max_loaded_connections: 16,
            upload_bytes_per_request: DEFAULT_UPLOAD_BYTES_PER_REQUEST,
            interval_duration_ms: 1000, // 1s
            test_duration_ms: 12_000,   // 12s
            disable_aim_scores: false,
        }
    }
}
