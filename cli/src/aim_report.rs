// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Structures and utilities for reporting data to Cloudflare's AIM aggregation.

use std::sync::Arc;

use http::{HeaderMap, HeaderValue};
use http_body_util::BodyExt;
use nq_core::client::Client;
use nq_core::{Time, TokioTime};
use nq_latency::LatencyResult;
use nq_rpm::{LoadedConnection, ResponsivenessResult};
use nq_tokio_network::TokioNetwork;
use serde::{Deserialize, Serialize};
use shellflip::ShutdownHandle;
use tracing::debug;

use crate::util::{pretty_ms, pretty_secs_to_ms};

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
    #[serde(skip)]
    origin: String,
}

impl CloudflareAimResults {
    pub fn from_rpm_results(
        rtt_result: &LatencyResult,
        download_result: &ResponsivenessResult,
        upload_result: &ResponsivenessResult,
        config_url: Option<String>,
    ) -> CloudflareAimResults {
        let latency_ms = rtt_result
            .measurements
            .values()
            .map(pretty_secs_to_ms)
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
            origin: config_url.unwrap_or_else(|| "https://rpm.speed.cloudflare.com".to_string()),
        }
    }

    pub async fn upload(&self) -> anyhow::Result<()> {
        let results = self.clone();
        let origin = self.origin.clone();

        let shutdown_handle = ShutdownHandle::default();
        let time = Arc::new(TokioTime::new());
        let network = Arc::new(TokioNetwork::new(
            Arc::clone(&time) as Arc<dyn Time>,
            Arc::new(shutdown_handle),
        ));

        let mut headers = HeaderMap::new();
        headers.append("Origin", HeaderValue::from_str(origin.as_str()).unwrap());
        headers.append("Content-Type", HeaderValue::from_static("application/json"));
        let body = serde_json::to_string(&results).unwrap();

        let response = Client::default()
            .new_connection(nq_core::ConnectionType::H2)
            .new_connection(nq_core::ConnectionType::H2)
            .headers(headers)
            .method("POST")
            .send(
                "https://aim.cloudflare.com/__log".parse().unwrap(),
                body,
                network,
                time,
            )?
            .await?;

        let (status, status_text) = (response.status(), response.status().to_string());
        let body = response.into_body().collect().await?.to_bytes();

        debug!(
            "aim upload response: {status} ({status_text}); {}",
            String::from_utf8_lossy(&body)
        );

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
        let throughput = rpm_result.throughput().unwrap_or(0) as f64;

        let bytes = throughput * rpm_result.duration.as_secs_f64();
        let bps = throughput as usize;

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
