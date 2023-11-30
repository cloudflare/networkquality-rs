use std::{fmt::Debug, future::Future, pin::Pin, sync::Arc};

use anyhow::Context;
use http::Request;
use http_body_util::BodyExt;
use nq_core::Network;
use nq_core::{ConnectionType, Speedtest, Time, Timestamp};
use nq_stats::TimeSeries;
use shellflip::ShutdownSignal;
use tracing::info;
use url::Url;

#[derive(Debug, Clone)]
pub struct RttConfig {
    pub url: Url,
    pub runs: usize,
}

impl Default for RttConfig {
    fn default() -> Self {
        Self {
            url: "https://aim.cloudflare.com/responsiveness/api/v1/small"
                .parse()
                .unwrap(),
            runs: 20,
        }
    }
}

pub struct Rtt {
    start: Timestamp,
    config: RttConfig,
    probe_results: TimeSeries,
}

impl Rtt {
    pub fn new(config: RttConfig) -> Self {
        Self {
            start: Timestamp::default(),
            config,
            probe_results: TimeSeries::new(),
        }
    }

    async fn run_test(
        mut self,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        _shutdown: ShutdownSignal,
    ) -> anyhow::Result<RttResult> {
        self.start = time.now();

        let mut handles = Vec::new();
        for run in 0..self.config.runs {
            let url = self.config.url.to_owned();
            let network = Arc::clone(&network);
            let time = Arc::clone(&time);

            let handle = tokio::spawn(async move {
                let host = url
                    .host_str()
                    .context("small download url must have a domain")?;
                let host_with_port =
                    format!("{}:{}", host, url.port_or_known_default().unwrap_or(443));

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
                    "latency run {run}: {:2.4}",
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

                Ok::<_, anyhow::Error>((conn_start, tcp_handshake_duration.as_secs_f64()))
            });
            handles.push(handle);
        }

        for handle in handles {
            let (conn_start, tcp_handshake_duration) = handle.await??;
            self.probe_results.add(conn_start, tcp_handshake_duration);
        }

        Ok(RttResult {
            measurements: self.probe_results,
        })
    }
}

impl Speedtest for Rtt {
    type TestResult = RttResult;

    fn run(
        self,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        shutdown: ShutdownSignal,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<RttResult>> + Send + 'static>> {
        Box::pin(Rtt::run_test(self, network, time, shutdown))
    }
}

#[derive(Default, Debug)]
pub struct RttResult {
    pub measurements: TimeSeries,
}
