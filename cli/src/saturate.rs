// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use nq_core::{Network, Time, TokioTime};
use nq_rpm::{Responsiveness, ResponsivenessConfig, ResponsivenessResult};
use nq_tokio_network::TokioNetwork;
use serde_json::json;
use tokio::join;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::args::saturate::{Direction, SaturateArgs, Urls};
use crate::util::pretty_secs;

/// Run a network saturation test. This is effectively an RPM test
/// with both TLS disabled AND no RPM measurements.
pub async fn run(cli_config: SaturateArgs) -> anyhow::Result<()> {
    debug!("running network saturation test with arguments {cli_config:?}");

    let Urls { upload, download } = cli_config.direction.urls();

    let config = ResponsivenessConfig {
        large_download_url: download.parse().context("parsing download url")?,
        small_download_url: download.parse().context("parsing download url")?,
        upload_url: upload.parse().context("parsing upload url")?,
        determine_load_only: true,
        conn_type: cli_config.conn_type.into(),
        test_duration: Duration::from_secs(cli_config.duration),
        ..Default::default()
    };

    let results = run_test(&config, &cli_config.direction)
        .await
        .context("running staturation test")?;

    let json = results.as_json();
    println!("{:#}", json);

    Ok(())
}

struct SaturateResults {
    download_result: Option<ResponsivenessResult>,
    upload_result: Option<ResponsivenessResult>,
}

impl SaturateResults {
    fn as_json(&self) -> serde_json::Value {
        fn build_json(res: &ResponsivenessResult) -> serde_json::Value {
            let dur = pretty_secs(res.duration.as_secs_f64());
            let mut capacity = res.capacity as u64;
            if capacity == 0 {
                capacity = res
                    .average_goodput_series
                    .quantile(0.90)
                    .unwrap_or_default() as u64;
            }

            json!({ "capacity": capacity, "dur": dur })
        }

        json!({
            "download": self.download_result.as_ref().map(build_json),
            "upload": self.upload_result.as_ref().map(build_json)
        })
    }
}

async fn run_test(
    config: &ResponsivenessConfig,
    direction: &Direction,
) -> anyhow::Result<SaturateResults> {
    match direction {
        Direction::Down { .. } => {
            info!(
                download_url = %config.large_download_url,
                "running download (ingress) network saturation test",
            );

            let download_result = run_single_test(config, true)
                .await
                .context("running download saturation test")?;

            Ok(SaturateResults {
                download_result: Some(download_result),
                upload_result: None,
            })
        }
        Direction::Up { .. } => {
            info!(
                upload_url = %config.upload_url,
                "running upload (egress) network saturation test",
            );

            let upload_result = run_single_test(config, false)
                .await
                .context("running upload saturation test")?;

            Ok(SaturateResults {
                download_result: None,
                upload_result: Some(upload_result),
            })
        }
        Direction::Both { .. } => {
            info!(
                download_url = %config.large_download_url,
                upload_url = %config.upload_url,
                "running parallel download/upload (ingress/egress) network saturation test"
            );

            let download_result_fut = run_single_test(config, true);
            let upload_result_fut = run_single_test(config, false);

            let (download_res, upload_res) = join!(download_result_fut, upload_result_fut);
            let download_result = download_res
                .inspect_err(|err| error!(%err, "error running download test"))
                .ok();
            let upload_result = upload_res
                .context("running upload saturation test")
                .inspect_err(|err| error!(%err, "error running download test"))
                .ok();

            Ok(SaturateResults {
                download_result,
                upload_result,
            })
        }
    }
}

async fn run_single_test(
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
