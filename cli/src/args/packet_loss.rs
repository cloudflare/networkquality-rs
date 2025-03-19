// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Arguments for running simple packet loss test.

use clap::Args;

/// Send UDP packets to a TURN server, reporting lost packets.
#[derive(Debug, Args)]
pub struct PacketLossArgs {
    /// The target TURN server URI to send UDP packets. User's will have to provide a TURN server.
    #[clap(short = 't', long)]
    pub turn_uri: String,
    /// The URL to send the request to for TURN server credentials
    #[clap(short = 'c', long)]
    pub turn_cred_url: String,
    /// Total number of messages/packets to send
    #[clap(default_value = "1000")]
    #[clap(short = 'p', long)]
    pub num_packets: usize,
    /// Total number of messages to send in a batch before waiting
    #[clap(default_value = "10")]
    #[clap(short = 's', long)]
    pub batch_size: usize,
    /// Time to wait between batch sends in milliseconds (ms).
    #[clap(default_value = "10")]
    #[clap(short = 'w', long)]
    pub batch_wait_time_ms: u64,
    /// Time to wait for receiving messages after all messages have been sent in milliseconds (ms).
    #[clap(default_value = "3000")]
    #[clap(short = 'r', long)]
    pub response_wait_time_ms: u64,

    /// The download file endpoint used for load generation which should be multiple GBs.
    #[clap(
        short = 'd',
        long = "download",
        default_value = "https://h3.speed.cloudflare.com/__down?bytes=10000000000"
    )]
    pub download_url: String,
    /// The upload url used for load generation which accepts an arbitrary amount of data.
    #[clap(
        short = 'u',
        long = "upload",
        default_value = "https://h3.speed.cloudflare.com/__up"
    )]
    pub upload_url: String,
}
