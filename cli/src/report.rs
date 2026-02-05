// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use anyhow::Context;
use nq_latency::LatencyResult;
use nq_rpm::ResponsivenessResult;
use serde::{Deserialize, Serialize};

use crate::util::{pretty_ms, pretty_secs_to_ms};

#[derive(Serialize, Deserialize)]
pub struct Report {
    unloaded_latency_ms: f64,
    // todo(fisher): implement packet loss from tcp info.
    // packet_loss: f64,
    jitter_ms: f64,

    download: RpmReport,
    upload: RpmReport,
}

impl Report {
    pub fn from_rtt_and_rpm_results(
        rtt_result: &LatencyResult,
        download_rpm_result: &ResponsivenessResult,
        upload_rpm_result: &ResponsivenessResult,
    ) -> anyhow::Result<Self> {
        let unloaded_latency_ms = rtt_result
            .median()
            .map(pretty_secs_to_ms)
            .context("no unloaded latency measurements")?;

        let jitter_ms = rtt_result.jitter().map(pretty_secs_to_ms).unwrap_or(0.0);

        let download =
            RpmReport::from_rpm_result(download_rpm_result).context("building download report")?;
        let upload =
            RpmReport::from_rpm_result(upload_rpm_result).context("building upload report")?;

        Ok(Report {
            unloaded_latency_ms,
            jitter_ms,
            download,
            upload,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct RpmReport {
    throughput: usize,
    loaded_latency_ms: f64,
    rpm: usize,
}

impl RpmReport {
    pub fn from_rpm_result(result: &ResponsivenessResult) -> anyhow::Result<RpmReport> {
        let throughput = result.throughput().context("no throughputs available")?;
        let loaded_latency_ms = match result.self_probe_latencies.quantile(0.5).map(pretty_ms) {
            Some(v) => v,
            None => {
                tracing::warn!("no loaded latency measurements; defaulting to 0ms");
                0.0
            }
        };

        Ok(RpmReport {
            throughput,
            loaded_latency_ms,
            rpm: result.rpm as usize,
        })
    }
}
