// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::sync::Arc;

use anyhow::Context;
use nq_core::client::{wait_for_finish, ThroughputClient};
use nq_core::{Network, Time, TokioTime};
use nq_tokio_network::TokioNetwork;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::args::up_down::{DownloadArgs, UploadArgs};
use crate::util::pretty_secs;

use serde_json::json;

/// Run a download test.
pub async fn download(args: DownloadArgs) -> anyhow::Result<()> {
    let shutdown = CancellationToken::new();
    let time = Arc::new(TokioTime::new()) as Arc<dyn Time>;
    let network =
        Arc::new(TokioNetwork::new(Arc::clone(&time), shutdown.clone())) as Arc<dyn Network>;

    let conn_type = args.conn_type.into();
    info!("downloading: {}", args.url);

    let inflight_body = ThroughputClient::download()
        .new_connection(conn_type)
        .send(
            args.url.parse().context("parsing download url")?,
            Arc::clone(&network),
            Arc::clone(&time),
            shutdown.clone(),
        )?
        .await?;

    let timing = inflight_body
        .timing
        .context("expected inflight body to have connection timing data")?;

    let body_start = time.now();
    let finished_result = wait_for_finish(inflight_body.events).await?;
    let finished = time.now();

    let time_body_raw = finished.duration_since(body_start);
    let time_total_raw = timing.time_secure() + time_body_raw;

    let dns_time = pretty_secs(timing.dns_time().as_secs_f64());
    let time_connect = pretty_secs(timing.time_connect().as_secs_f64());
    let time_secure = pretty_secs(timing.time_secure().as_secs_f64());
    let time_body = pretty_secs(time_body_raw.as_secs_f64());
    let time_total = pretty_secs(time_total_raw.as_secs_f64());
    let bytes_total = finished_result.total;
    let throughput = ((finished_result.total as f64 * 8.0) / time_total_raw.as_secs_f64()) as u64;

    let json = json!({
        "dns_time":     dns_time,
        "time_connect": time_connect,
        "time_secure":  time_secure,
        "time_body":    time_body,
        "time_total":   time_total,
        "bytes_total":  bytes_total,
        "throughput":   throughput,
    });

    println!("{:#}", json);

    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
        shutdown.cancel();
    })
    .await;

    Ok(())
}

/// Run a download test.
// todo(fisher): investigate body completion events. Moving to Socket stats is
// likely the best option.
#[allow(dead_code)]
pub async fn upload(args: UploadArgs) -> anyhow::Result<()> {
    let shutdown = CancellationToken::new();
    let time = Arc::new(TokioTime::new()) as Arc<dyn Time>;
    let network =
        Arc::new(TokioNetwork::new(Arc::clone(&time), shutdown.clone())) as Arc<dyn Network>;

    let conn_type = args.conn_type.into();
    let bytes = args.bytes.unwrap_or(10_000_000);

    println!("{}\n", args.url);

    let inflight_body = ThroughputClient::upload(bytes)
        .new_connection(conn_type)
        .send(
            args.url.parse()?,
            Arc::clone(&network),
            Arc::clone(&time),
            shutdown.clone(),
        )?
        .await?;

    let timing = inflight_body
        .timing
        .context("expected inflight body to have connection timing data")?;

    let body_start = time.now();
    let finished_result = wait_for_finish(inflight_body.events).await?;
    let finished = time.now();

    let time_body_raw = finished.duration_since(body_start);
    let time_total_raw = timing.time_secure() + time_body_raw;

    let dns_time = pretty_secs(timing.dns_time().as_secs_f64());
    let time_connect = pretty_secs(timing.time_connect().as_secs_f64());
    let time_secure = pretty_secs(timing.time_secure().as_secs_f64());
    let time_body = pretty_secs(time_body_raw.as_secs_f64());
    let time_total = pretty_secs(time_total_raw.as_secs_f64());
    let bytes_total = finished_result.total;
    let throughput = ((finished_result.total as f64 * 8.0) / time_total_raw.as_secs_f64()) as u64;

    let json = json!({
        "dns_time":     dns_time,
        "time_connect": time_connect,
        "time_secure":  time_secure,
        "time_body":    time_body,
        "time_total":   time_total,
        "bytes_total":  bytes_total,
        "throughput":   throughput,
    });

    println!("{:#}", json);

    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
        shutdown.cancel();
    })
    .await;

    Ok(())
}
