// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::sync::Arc;
use std::time::Duration;

use crate::nq_core::client::Client;
use crate::nq_core::{ConnectionType, Network, Time, TokioTime};
use crate::nq_latency::LatencyConfig;
use crate::nq_rpm::{Responsiveness, ResponsivenessConfig, ResponsivenessResult};
use crate::nq_tokio_network::TokioNetwork;
use anyhow::{Context, bail};
use http_body_util::BodyExt;
use serde::Deserialize;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::aim_report::CloudflareAimResults;
use crate::args::rpm::{RpmArgs, SMALL_UPLOAD_BYTES_PER_REQUEST};
use crate::report::Report;
use crate::util::pretty_secs_to_ms;

/// Warn loudly when a leg lost load-generating connections.
///
/// A failed load-generating connection means the link was not fully loaded for
/// part of the run, so the RPM score for that leg is measured under weaker
/// working conditions than intended and reads too high. That is far more
/// dangerous than an outright error, because the result still looks like a
/// perfectly ordinary number -- so it needs saying out loud rather than only
/// appearing as a JSON field.
fn warn_on_degraded_result(leg: &str, failed_connections: usize, upload_bytes_per_request: usize) {
    if failed_connections == 0 {
        return;
    }

    warn!(
        "{leg}: {failed_connections} load-generating connection(s) failed, so the link was not \
         fully loaded for part of the test -- treat this {leg} RPM score as unreliable (it will \
         read higher than the truth)"
    );

    if leg == "upload" {
        warn!(
            "if these were HTTP 413 rejections, the server caps request bodies below the current \
             --upload-max-request-bytes ({upload_bytes_per_request}); try a lower value"
        );
    }
}

/// Run a responsiveness test.
pub async fn run(cli_config: RpmArgs) -> anyhow::Result<()> {
    info!("running responsiveness test");

    let scoped_headers = crate::access::cf_access_scoped_headers()?;

    if cli_config.insecure {
        warn!("TLS certificate verification disabled (--insecure); do not use against production");
        crate::nq_core::set_insecure_tls(true);
    }

    // Copied out before `cli_config` is partially moved building the URL list.
    let upload_bytes_per_request = cli_config.upload_bytes_per_request;

    let rpm_urls = match cli_config.config.clone() {
        Some(endpoint) => {
            info!("fetching configuration from {endpoint}");
            let urls = get_rpm_config(endpoint, scoped_headers.clone()).await?;
            info!("retrieved configuration urls: {urls:?}");

            urls
        }
        None => {
            let urls = RpmUrls {
                small_download_url: cli_config.small_download_url,
                large_download_url: cli_config.large_download_url,
                upload_url: cli_config.upload_url,
            };
            info!("using default configuration urls: {urls:?}");

            urls
        }
    };

    // first get unloaded RTT measurements
    info!("determining unloaded latency");
    let rtt_result = crate::latency::run_test(&LatencyConfig {
        url: rpm_urls.small_download_url.parse()?,
        runs: 20,
        scoped_headers: scoped_headers.clone(),
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
        large_download_url: rpm_urls.large_download_url.parse()?,
        small_download_url: rpm_urls.small_download_url.parse()?,
        upload_url: rpm_urls.upload_url.parse()?,
        moving_average_distance: cli_config.moving_average_distance,
        interval_duration: Duration::from_millis(cli_config.interval_duration_ms),
        test_duration: Duration::from_millis(cli_config.test_duration_ms),
        trimmed_mean_percent: cli_config.trimmed_mean_percent,
        std_tolerance: cli_config.std_tolerance,
        max_loaded_connections: cli_config.max_loaded_connections,
        conn_type: ConnectionType::H2,
        determine_load_only: false,
        upload_bytes_per_request: cli_config.upload_bytes_per_request,
        on_connection_error: cli_config.on_connection_error.into(),
        scoped_headers,
    };

    if cli_config.upload_bytes_per_request < SMALL_UPLOAD_BYTES_PER_REQUEST {
        warn!(
            "--upload-max-request-bytes is {} ({} MiB); request overhead becomes significant \
             at this size and the upload leg may under-report capacity",
            cli_config.upload_bytes_per_request,
            cli_config.upload_bytes_per_request / (1024 * 1024),
        );
    }

    info!("running download test");
    let download_result = run_test(&config, true).await?;
    debug!("download result={download_result:?}");

    info!("running upload test");
    let upload_result = run_test(&config, false).await?;
    debug!("upload result={upload_result:?}");

    warn_on_degraded_result(
        "download",
        download_result.failed_connections,
        upload_bytes_per_request,
    );
    warn_on_degraded_result(
        "upload",
        upload_result.failed_connections,
        upload_bytes_per_request,
    );

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

    let upload_timeout_secs = std::env::var("MACH_UPLOAD_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let _ = timeout(Duration::from_secs(upload_timeout_secs), upload_handle).await;

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

/// Server config as published at a responsiveness config endpoint.
///
/// Servers name each URL either with the spec key (`small_download_url`) or
/// the legacy `https` key (`small_https_download_url`); Cloudflare ships both,
/// Apple only the spec keys. Resolved into [`RpmUrls`] by [`parse_rpm_config`].
#[derive(Deserialize)]
struct RpmServerConfig {
    urls: RpmServerUrls,
}

#[derive(Deserialize)]
struct RpmServerUrls {
    small_download_url: Option<String>,
    small_https_download_url: Option<String>,
    large_download_url: Option<String>,
    large_https_download_url: Option<String>,
    upload_url: Option<String>,
    https_upload_url: Option<String>,
}

#[derive(Debug)]
pub struct RpmUrls {
    small_download_url: String,
    large_download_url: String,
    upload_url: String,
}

/// Pick the legacy `https` key when present, else the spec key.
fn resolve_url(
    https: Option<String>,
    plain: Option<String>,
    https_key: &str,
    plain_key: &str,
) -> anyhow::Result<String> {
    match https.or(plain) {
        Some(url) => Ok(url),
        None => bail!("rpm config is missing `{plain_key}` (or `{https_key}`)"),
    }
}

fn parse_rpm_config(body: &[u8]) -> anyhow::Result<RpmUrls> {
    let config: RpmServerConfig =
        serde_json::from_slice(body).context("parsing json config from rpm url")?;
    let urls = config.urls;

    Ok(RpmUrls {
        small_download_url: resolve_url(
            urls.small_https_download_url,
            urls.small_download_url,
            "small_https_download_url",
            "small_download_url",
        )?,
        large_download_url: resolve_url(
            urls.large_https_download_url,
            urls.large_download_url,
            "large_https_download_url",
            "large_download_url",
        )?,
        upload_url: resolve_url(
            urls.https_upload_url,
            urls.upload_url,
            "https_upload_url",
            "upload_url",
        )?,
    })
}

pub async fn get_rpm_config(
    config_url: String,
    scoped_headers: Option<crate::nq_core::ScopedHeaders>,
) -> anyhow::Result<RpmUrls> {
    let shutdown = CancellationToken::new();
    let time = Arc::new(TokioTime::new());
    let network = Arc::new(TokioNetwork::new(
        Arc::clone(&time) as Arc<dyn Time>,
        shutdown.clone(),
    ));

    let client = Client::default()
        .new_connection(ConnectionType::H2)
        .method("GET")
        .scoped_headers(scoped_headers);

    let response = client
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

    parse_rpm_config(&response.into_body().collect().await?.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim body of https://mensura.cdn-apple.com/.well-known/nq (issue #53).
    const APPLE_CONFIG: &str = r#"{ "version": 1,
  "test_endpoint": "esmad6-edge-fx-028.aaplimg.com",
  "urls": {
      "small_download_url": "https://mensura.cdn-apple.com/api/v1/gm/small",
      "large_download_url": "https://mensura.cdn-apple.com/api/v1/gm/large",
      "upload_url": "https://mensura.cdn-apple.com/api/v1/gm/slurp"
   }
}"#;

    #[test]
    fn spec_only_keys_resolve() {
        let urls = parse_rpm_config(APPLE_CONFIG.as_bytes()).unwrap();
        assert_eq!(
            urls.small_download_url,
            "https://mensura.cdn-apple.com/api/v1/gm/small"
        );
        assert_eq!(
            urls.large_download_url,
            "https://mensura.cdn-apple.com/api/v1/gm/large"
        );
        assert_eq!(
            urls.upload_url,
            "https://mensura.cdn-apple.com/api/v1/gm/slurp"
        );
    }

    #[test]
    fn https_keys_take_precedence() {
        let body = r#"{"urls": {
            "small_download_url": "http://a/small",
            "small_https_download_url": "https://a/small",
            "large_download_url": "http://a/large",
            "large_https_download_url": "https://a/large",
            "upload_url": "http://a/up",
            "https_upload_url": "https://a/up"
        }}"#;
        let urls = parse_rpm_config(body.as_bytes()).unwrap();
        assert_eq!(urls.small_download_url, "https://a/small");
        assert_eq!(urls.large_download_url, "https://a/large");
        assert_eq!(urls.upload_url, "https://a/up");
    }

    #[test]
    fn missing_url_names_both_keys() {
        let body = r#"{"urls": {
            "small_download_url": "https://a/small",
            "large_https_download_url": "https://a/large"
        }}"#;
        let err = parse_rpm_config(body.as_bytes()).unwrap_err().to_string();
        assert!(err.contains("`upload_url`"), "{err}");
        assert!(err.contains("`https_upload_url`"), "{err}");
    }
}
