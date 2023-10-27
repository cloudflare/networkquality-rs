//! Structures and utilities for reporting data to Cloudflare's AIM aggregation.

use serde::{Deserialize, Serialize};

/// Describes the format of Cloudflare AIM results uploaded with test runs.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareAimResults {
    pub(crate) latency_ms: Vec<f64>,
    pub(crate) download: Vec<BpsMeasurement>,
    pub(crate) upload: Vec<BpsMeasurement>,
    pub(crate) down_loaded_latency_ms: Vec<f64>,
    pub(crate) up_loaded_latency_ms: Vec<f64>,
    pub(crate) packet_loss: PacketLoss,
}

/// Describes the bits/s of some transfer.
#[derive(Serialize, Deserialize)]
pub struct BpsMeasurement {
    /// The total number of bytes.
    bytes: usize,
    /// The bits per second of the transfer.
    bps: usize,
}

/// A measure of packet loss.
#[derive(Serialize, Deserialize)]
pub struct PacketLoss {
    num_messages: usize,
    loss_ration: f64,
}

/// Default to 0% packet loss
// todo(fisher): add network statistics as a part of the `Network` trait.
impl Default for PacketLoss {
    fn default() -> Self {
        Self {
            num_messages: 1000,
            loss_ration: 0.0,
        }
    }
}

/// Calculated AIM scores:
/// https://developers.cloudflare.com/speed/aim/
#[derive(Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum AimScore {
    Streaming {
        points: usize,
        classification: String,
    },
    Gaming {
        points: usize,
        classification: String,
    },
    Rtc {
        points: usize,
        classification: String,
    },
}
