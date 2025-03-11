// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use anyhow::{bail, Context};
use http::{HeaderMap, HeaderValue};
use http_body_util::BodyExt;
use nq_core::{client::Client, ConnectionType, Time, TokioTime};
use nq_packetloss::{PacketLoss, PacketLossConfig, TurnServerCreds};
use nq_tokio_network::TokioNetwork;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::args::packet_loss::PacketLossArgs;

pub async fn run(args: PacketLossArgs) -> anyhow::Result<()> {
    info!("running packet loss test");

    let shutdown = CancellationToken::new();
    let config = PacketLossConfig {
        turn_server_uri: args.turn_uri,
        turn_cred_request_url: args.turn_cred_url.parse()?,
        num_packets: args.num_packets,
        batch_size: args.batch_size,
        batch_wait_time: Duration::from_millis(args.batch_wait_time_ms),
        response_wait_time: Duration::from_millis(args.response_wait_time_ms),
        download_url: args.download_url.parse()?,
        upload_url: args.upload_url.parse()?,
    };

    info!("fetching TURN server credentials");
    let turn_server_creds = fetch_turn_server_creds(&config, shutdown.clone()).await?;

    info!("sending {} UDP packets to TURN server", config.num_packets);
    let packet_loss = PacketLoss::new_with_config(config)?;
    let packet_loss_result = packet_loss.run_test(turn_server_creds, shutdown).await?;

    println!("{}", serde_json::to_string_pretty(&packet_loss_result)?);
    Ok(())
}

/// Fetch the TURN creds from the configured HTTP server
async fn fetch_turn_server_creds(
    config: &PacketLossConfig,
    shutdown: CancellationToken,
) -> anyhow::Result<TurnServerCreds> {
    let request_url = config.turn_cred_request_url.clone();
    let time = Arc::new(TokioTime::new());
    let network = Arc::new(TokioNetwork::new(
        Arc::clone(&time) as Arc<dyn Time>,
        shutdown.clone(),
    ));

    let host = config
        .turn_cred_request_url
        .host_str()
        .ok_or(anyhow::anyhow!("url has no host"))?;
    let mut headers = HeaderMap::new();
    headers.append(hyper::header::HOST, HeaderValue::from_str(host)?);

    let response = Client::default()
        .new_connection(ConnectionType::H1)
        .method("GET")
        .headers(headers)
        .send(
            request_url.to_string().parse()?,
            http_body_util::Empty::new(),
            network,
            time,
        )?
        .await?;

    if !response.status().is_success() {
        bail!(
            "could not fetch turn credentials from: {request_url} {}",
            response.status()
        );
    }

    let creds = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())
        .context("parsing json creds from turn server url")?;
    Ok(creds)
}