//! Structures and utilities for reporting data to Cloudflare's AIM aggregation.

use nq_core::client::MACH_USER_AGENT;
use nq_rpm::{LoadedConnection, ResponsivenessResult};
use nq_rtt::RttResult;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::util::pretty_ms;

/// Describes the format of Cloudflare AIM results uploaded with test runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareAimResults {
    pub(crate) latency_ms: Vec<f64>,
    pub(crate) download: Vec<BpsMeasurement>,
    pub(crate) upload: Vec<BpsMeasurement>,
    pub(crate) down_loaded_latency_ms: Vec<f64>,
    pub(crate) up_loaded_latency_ms: Vec<f64>,
    pub(crate) packet_loss: PacketLoss,
    pub(crate) responsiveness: f64,
}

impl CloudflareAimResults {
    pub fn from_rpm_results(
        rtt_result: &RttResult,
        download_result: &ResponsivenessResult,
        upload_result: &ResponsivenessResult,
    ) -> CloudflareAimResults {
        let latency_ms = rtt_result
            .measurements
            .values()
            .map(|ms| ms * 1_000.0)
            .collect();

        let mut download =
            BpsMeasurement::from_loaded_connections(&download_result.loaded_connections);
        download.push(BpsMeasurement::from_rpm_result(download_result));

        let mut upload = BpsMeasurement::from_loaded_connections(&upload_result.loaded_connections);
        upload.push(BpsMeasurement::from_rpm_result(upload_result));

        let down_loaded_latency_ms = download_result
            .self_probe_latencies
            .values()
            .map(pretty_ms)
            .collect();
        let up_loaded_latency_ms = upload_result
            .self_probe_latencies
            .values()
            .map(pretty_ms)
            .collect();

        let packet_loss = PacketLoss {
            num_messages: 0,
            loss_ratio: 0.0,
        };

        CloudflareAimResults {
            latency_ms,
            download,
            upload,
            down_loaded_latency_ms,
            up_loaded_latency_ms,
            packet_loss,
            responsiveness: download_result.rpm,
        }
    }

    pub async fn upload(&self) -> anyhow::Result<()> {
        let results = self.clone();

        let response = tokio::task::spawn_blocking(move || {
            match ureq::post("https://aim.cloudflare.com/__log")
                .set("User-Agent", MACH_USER_AGENT)
                .set("Origin", "https://speed.cloudflare.com")
                .send_json(results)
            {
                Err(ureq::Error::Status(_, resp)) => Ok(resp),
                resp => resp,
            }
        })
        .await??;

        let (status, status_text) = (response.status(), response.status_text().to_string());
        let body = response.into_string()?;

        info!("aim upload response: {status} ({status_text}); {body}");

        if status != 200 {
            anyhow::bail!("error uploading aim results");
        }

        Ok(())
    }
}

/// Describes the bits/s of some transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BpsMeasurement {
    /// The total number of bytes.
    bytes: usize,
    /// The bits per second of the transfer.
    bps: usize,
}

impl BpsMeasurement {
    fn from_loaded_connections(connections: &[LoadedConnection]) -> Vec<BpsMeasurement> {
        connections
            .iter()
            .map(|connection| {
                let bytes = connection.total_bytes_series().sum();
                let bps = connection.total_bytes_series().average().unwrap_or(0.0);

                BpsMeasurement {
                    bytes: bytes as usize,
                    bps: bps as usize,
                }
            })
            .collect()
    }

    /// Use the test duration and network capacity to create a synthetic bps result.
    fn from_rpm_result(rpm_result: &ResponsivenessResult) -> BpsMeasurement {
        let bytes = rpm_result.capacity * rpm_result.duration.as_secs_f64();
        let bps = rpm_result.capacity as usize;

        BpsMeasurement {
            bytes: bytes as usize,
            bps,
        }
    }
}

/// A measure of packet loss.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PacketLoss {
    num_messages: usize,
    loss_ratio: f64,
}

/// Default to 0% packet loss
// todo(fisher): add network statistics as a part of the `Network` trait.
impl Default for PacketLoss {
    fn default() -> Self {
        Self {
            num_messages: 1000,
            loss_ratio: 0.0,
        }
    }
}

/// Calculated AIM scores:
/// https://developers.cloudflare.com/speed/aim/
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
