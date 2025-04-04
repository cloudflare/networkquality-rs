// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use http_body_util::BodyExt;
use nq_core::client::Client;
use nq_core::{ConnectionType, Network, Time, TokioTime};
use nq_latency::LatencyConfig;
use nq_rpm::{Responsiveness, ResponsivenessConfig, ResponsivenessResult};
use nq_tokio_network::TokioNetwork;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::aim_report::CloudflareAimResults;
use crate::args::rpm::RpmArgs;
use crate::report::Report;
use crate::util::pretty_secs_to_ms;

/// Run a responsiveness test.
pub async fn run(cli_config: RpmArgs) -> anyhow::Result<()> {
    info!("running responsiveness test");

    let rpm_urls = match cli_config.config.clone() {
        Some(endpoint) => {
            info!("fetching configuration from {endpoint}");
            let urls = get_rpm_config(endpoint).await?.urls;
            info!("retrieved configuration urls: {urls:?}");

            urls
        }
        None => {
            let urls = RpmUrls {
                small_https_download_url: cli_config.small_download_url,
                large_https_download_url: cli_config.large_download_url,
                https_upload_url: cli_config.upload_url,
            };
            info!("using default configuration urls: {urls:?}");

            urls
        }
    };

    // first get unloaded RTT measurements
    info!("determining unloaded latency");
    let rtt_result = crate::latency::run_test(&LatencyConfig {
        url: rpm_urls.small_https_download_url.parse()?,
        runs: 20,
    })
    .await?;
    info!(
        "unloaded latency: {} ms. jitter: {} ms",
        rtt_result
            .median()
            .map(pretty_secs_to_ms)
            .unwrap_or_default(),
        rtt_result
            .jitter()
            .map(pretty_secs_to_ms)
            .unwrap_or_default(),
    );

    let config = ResponsivenessConfig {
        large_download_url: rpm_urls.large_https_download_url.parse()?,
        small_download_url: rpm_urls.small_https_download_url.parse()?,
        upload_url: rpm_urls.https_upload_url.parse()?,
        moving_average_distance: cli_config.moving_average_distance,
        interval_duration: Duration::from_millis(cli_config.interval_duration_ms),
        test_duration: Duration::from_millis(cli_config.test_duration_ms),
        trimmed_mean_percent: cli_config.trimmed_mean_percent,
        std_tolerance: cli_config.std_tolerance,
        max_loaded_connections: cli_config.max_loaded_connections,
    };

    info!("running download test");
    let download_result = run_test(&config, true).await?;
    debug!("download result={download_result:?}");

    info!("running upload test");
    let upload_result = run_test(&config, false).await?;
    debug!("upload result={upload_result:?}");

    let aim_results = CloudflareAimResults::from_rpm_results(
        &rtt_result,
        &download_result,
        &upload_result,
        cli_config.config,
    );

    let upload_handle = tokio::spawn(async move {
        if !cli_config.disable_aim_scores {
            debug!("uploading aim report");
            if let Err(e) = aim_results.upload().await {
                error!("error uploading aim results: {e}");
            }
        }
    });

    info!("generating rpm report");
    let report = Report::from_rtt_and_rpm_results(&rtt_result, &download_result, &upload_result)
        .context("building RPM report")?;

    println!("{}", serde_json::to_string_pretty(&report)?);

    let _ = timeout(Duration::from_secs(1), upload_handle).await;

    Ok(())
}

async fn run_test(
    config: &ResponsivenessConfig,
    download: bool,
) -> anyhow::Result<ResponsivenessResult> {
    let shutdown = CancellationToken::new();
    let time = Arc::new(TokioTime::new()) as Arc<dyn Time>;
    let network =
        Arc::new(TokioNetwork::new(Arc::clone(&time), shutdown.clone())) as Arc<dyn Network>;

    let rpm = Responsiveness::new(config.clone(), download)?;
    let result = rpm.run_test(network, time, shutdown.clone()).await?;

    debug!("shutting down rpm test");
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
        shutdown.cancel();
    })
    .await;

    Ok(result)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpmServerConfig {
    urls: RpmUrls,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpmUrls {
    #[serde(alias = "small_download_url")]
    small_https_download_url: String,
    #[serde(alias = "large_download_url")]
    large_https_download_url: String,
    #[serde(alias = "upload_url")]
    https_upload_url: String,
}

pub async fn get_rpm_config(config_url: String) -> anyhow::Result<RpmServerConfig> {
    let shutdown = CancellationToken::new();
    let time = Arc::new(TokioTime::new());
    let network = Arc::new(TokioNetwork::new(
        Arc::clone(&time) as Arc<dyn Time>,
        shutdown.clone(),
    ));

    let response = Client::default()
        .new_connection(ConnectionType::H2)
        .method("GET")
        .send(
            config_url.parse().context("parsing rpm config url")?,
            http_body_util::Empty::new(),
            network,
            time,
        )?
        .await?;

    if !response.status().is_success() {
        bail!("could not fetch rpm config from: {config_url}");
    }

    let json = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())
        .context("parsing json config from rpm url")?;

    Ok(json)
}
