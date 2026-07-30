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
    /// Load-generating connections that terminated early with an error.
    /// Non-zero means the link was not fully loaded for part of the run, so
    /// this result is degraded and should not be compared with a clean one.
    #[serde(skip_serializing_if = "is_zero")]
    failed_connections: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl RpmReport {
    pub fn from_rpm_result(result: &ResponsivenessResult) -> anyhow::Result<RpmReport> {
        Ok(RpmReport {
            throughput: result.throughput().context("no throughputs available")?,
            loaded_latency_ms: result
                .self_probe_latencies
                .quantile(0.5)
                .map(pretty_ms)
                .context("no loaded latency measurements")?,
            // Absent RPM is a failure to measure, reported the same way as an
            // absent throughput just above rather than as a zero.
            rpm: result
                .rpm
                .context("no rpm measurements: no interval produced a responsiveness sample")?
                as usize,
            failed_connections: result.failed_connections,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nq_core::Timestamp;

    /// A result with throughput and loaded latency present, so that the only
    /// thing under test is how `rpm` is handled.
    fn result_with_rpm(rpm: Option<f64>) -> ResponsivenessResult {
        let mut result = ResponsivenessResult {
            rpm,
            ..Default::default()
        };
        let at = Timestamp::now();
        result.average_goodput_series.add(at, 104_000_000.0);
        result.self_probe_latencies.add(at, 0.231);
        result
    }

    #[test]
    fn absent_rpm_fails_the_report_instead_of_serializing_zero() {
        // The whole point of making `rpm` an Option: a run that measured no
        // responsiveness must not publish a plausible-looking number. Previously
        // this path emitted `"rpm": 0`, which is indistinguishable from a real
        // measurement of a badly bufferbloated link.
        let err = match RpmReport::from_rpm_result(&result_with_rpm(None)) {
            Ok(_) => panic!("absent rpm must not produce a report"),
            Err(err) => err,
        };
        assert!(
            format!("{err:#}").contains("no rpm measurements"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn present_rpm_is_reported_unchanged() {
        let report = RpmReport::from_rpm_result(&result_with_rpm(Some(347.9)))
            .expect("a measured rpm should report");
        let json = serde_json::to_string(&report).expect("serializing report");
        assert!(json.contains("\"rpm\":347"), "got {json}");
    }
}
