// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::{fmt::Debug, sync::Arc};

use anyhow::Context;
use http::Request;
use http_body_util::BodyExt;
use nq_core::Network;
use nq_core::{ConnectionType, Time, Timestamp};
use nq_stats::TimeSeries;
use tokio_util::sync::CancellationToken;
use tracing::info;
use url::Url;

#[derive(Debug, Clone)]
pub struct LatencyConfig {
    pub url: Url,
    pub runs: usize,
    /// Disable TLS for H1 connections (plain TCP). Used when higher level
    /// command passes the `--no-tls` flag.
    pub no_tls: bool,
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            url: "https://h3.speed.cloudflare.com/__down?bytes=10"
                .parse()
                .unwrap(),
            runs: 20,
            no_tls: false,
        }
    }
}

pub struct Latency {
    start: Timestamp,
    config: LatencyConfig,
    probe_results: TimeSeries,
}

impl Latency {
    pub fn new(config: LatencyConfig) -> Self {
        Self {
            start: Timestamp::now(),
            config,
            probe_results: TimeSeries::new(),
        }
    }

    pub async fn run_test(
        mut self,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        _shutdown: CancellationToken,
    ) -> anyhow::Result<LatencyResult> {
        self.start = time.now();

        for run in 0..self.config.runs {
            let url = self.config.url.to_owned();
            let network = Arc::clone(&network);
            let time = Arc::clone(&time);

            let host = url
                .host_str()
                .context("small download url must have a domain")?;
            // Use explicit default ports based on scheme: 443 for HTTPS, 80 for HTTP, else fallback to 443.
            let default_port = match url.scheme() {
                "https" => 443,
                "http" => 80,
                _ => 443,
            };
            let port = url.port().unwrap_or(default_port);
            let host_with_port = format!("{}:{}", host, port);

            let conn_start = time.now();

            let addrs = network
                .resolve(host_with_port)
                .await
                .context("unable to resolve host")?;
            let time_lookup = time.now();

            // Use H1 only when TLS is disabled; otherwise prefer H2.
            let conn_type = if self.config.no_tls { ConnectionType::H1 } else { ConnectionType::H2 };
            let connection = network
                .new_connection(conn_start, addrs[0], host.to_string(), conn_type)
                .await
                .context("unable to create new connection")?;
            {
                let conn = connection.write().await;
                conn.timing().set_lookup(time_lookup);
            }

            let tcp_handshake_duration = {
                let conn = connection.read().await;
                conn.timing()
                    .time_connect()
                    .saturating_sub(conn.timing().time_lookup())
            };

            info!(
                "latency run {run}: {:2.4} s.",
                tcp_handshake_duration.as_secs_f32()
            );

            // perform a simple GET to do some amount of work
            let response = network
                .send_request(
                    connection,
                    Request::get(url.as_str()).body(Default::default())?,
                )
                .await
                .context("GET request failed")?;

            let _ = response
                .into_body()
                .collect()
                .await
                .context("unable to read GET request body")?;

            self.probe_results
                .add(conn_start, tcp_handshake_duration.as_secs_f64());
        }

        // while let Some(res) = task_set.join_next().await {
        //     let (conn_start, tcp_handshake_duration) = res??;
        //     self.probe_results.add(conn_start, tcp_handshake_duration);
        // }

        Ok(LatencyResult {
            measurements: self.probe_results,
        })
    }
}

#[derive(Default, Debug)]
pub struct LatencyResult {
    pub measurements: TimeSeries,
}

impl LatencyResult {
    pub fn median(&self) -> Option<f64> {
        self.measurements.quantile(0.50)
    }

    /// Jitter as calculated as the average distance between consecutive rtt
    /// measurments.
    pub fn jitter(&self) -> Option<f64> {
        let values: Vec<_> = self.measurements.values().collect();

        let distances = values.windows(2).filter_map(|window| {
            window
                .last()
                .zip(window.first())
                .map(|(last, first)| (last - first).abs())
        });

        let sum: f64 = distances.clone().sum();
        let count = distances.count();

        if count > 0 {
            Some(sum / count as f64)
        } else {
            None
        }
    }
}
