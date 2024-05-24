use std::{fmt::Debug, sync::Arc};

use anyhow::Context;
use http::Request;
use http_body_util::BodyExt;
use nq_core::Network;
use nq_core::{ConnectionType, Time, Timestamp};
use nq_stats::TimeSeries;
use shellflip::ShutdownSignal;
use tracing::info;
use url::Url;

#[derive(Debug, Clone)]
pub struct LatencyConfig {
    pub url: Url,
    pub runs: usize,
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            url: "https://aim.cloudflare.com/responsiveness/api/v1/small"
                .parse()
                .unwrap(),
            runs: 20,
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
        _shutdown: ShutdownSignal,
    ) -> anyhow::Result<LatencyResult> {
        self.start = time.now();

        for run in 0..self.config.runs {
            let url = self.config.url.to_owned();
            let network = Arc::clone(&network);
            let time = Arc::clone(&time);

            let host = url
                .host_str()
                .context("small download url must have a domain")?;
            let host_with_port = format!("{}:{}", host, url.port_or_known_default().unwrap_or(443));

            let conn_start = time.now();

            let addrs = network
                .resolve(host_with_port)
                .await
                .context("unable to resolve host")?;
            let time_lookup = time.now();

            let mut connection = network
                .new_connection(conn_start, addrs[0], host.to_string(), ConnectionType::H1)
                .await
                .context("unable to create new connection")?;

            connection.timing.set_lookup(time_lookup);

            let tcp_handshake_duration = connection
                .timing
                .time_connect()
                .saturating_sub(connection.timing.time_lookup());

            info!(
                "latency run {run}: {:2.4} s.",
                tcp_handshake_duration.as_secs_f32()
            );

            // perform a simple GET to do some amount of work
            let response = network
                .send_request(
                    connection.id,
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
