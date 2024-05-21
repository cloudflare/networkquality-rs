use std::sync::Arc;

use anyhow::Context;
use nq_core::client::{wait_for_finish, ThroughputClient};
use nq_core::{ConnectionType, Network, Time, TokioTime};
use nq_tokio_network::TokioNetwork;
use shellflip::{ShutdownCoordinator, ShutdownSignal};
use tracing::info;

use crate::args::up_down::{DownloadArgs, UploadArgs};
use crate::args::ConnType;

/// Run a download test.
pub async fn download(args: DownloadArgs) -> anyhow::Result<()> {
    let shutdown_coordinator = ShutdownCoordinator::default();
    let time = Arc::new(TokioTime::new()) as Arc<dyn Time>;
    let network = Arc::new(TokioNetwork::new(
        Arc::clone(&time),
        shutdown_coordinator.handle(),
    )) as Arc<dyn Network>;

    let conn_type = match args.conn_type {
        ConnType::H1 => ConnectionType::H1,
        ConnType::H2 => ConnectionType::H2,
        ConnType::H3 => unimplemented!("H3 is not yet implemented"), // ConnectionType::H3,
    };

    info!("downloading: {}", args.url);

    let inflight_body = ThroughputClient::download()
        .new_connection(conn_type)
        .send(
            args.url.parse().context("parsing download url")?,
            Arc::clone(&network),
            Arc::clone(&time),
            ShutdownSignal::from(&*shutdown_coordinator.handle()),
        )?
        .await?;

    let timing = inflight_body
        .timing
        .context("expected inflight body to have connection timing data")?;

    let body_start = time.now();
    let finished_result = wait_for_finish(inflight_body.events).await?;
    let finished = time.now();

    let time_body = finished.duration_since(body_start);
    let time_total = timing.time_secure() + time_body;

    println!(" time_lookup: {:.4}", timing.time_lookup().as_secs_f32());
    println!("time_connect: {:.4}", timing.time_connect().as_secs_f32());
    println!(" time_secure: {:.4}", timing.time_secure().as_secs_f32());
    println!(
        "   time_body: {:.4}",
        finished.duration_since(body_start).as_secs_f32()
    );
    println!("  time_total: {:.4}", time_total.as_secs_f32());
    println!();
    println!(" bytes_total: {}", finished_result.total);
    println!(
        "         bps: {:.4}",
        (finished_result.total * 8) as f32 / time_total.as_secs_f32()
    );

    let _ = shutdown_coordinator.shutdown_with_timeout(1).await;

    Ok(())
}

/// Run a download test.
// todo(fisher): investigate body completion events. Moving to Socket stats is
// likely the best option.
#[allow(dead_code)]
pub async fn upload(args: UploadArgs) -> anyhow::Result<()> {
    let shutdown_coordinator = ShutdownCoordinator::default();
    let time = Arc::new(TokioTime::new()) as Arc<dyn Time>;
    let network = Arc::new(TokioNetwork::new(
        Arc::clone(&time),
        shutdown_coordinator.handle(),
    )) as Arc<dyn Network>;

    let conn_type = match args.conn_type {
        ConnType::H1 => ConnectionType::H1, // ConnectionType::H1,
        ConnType::H2 => ConnectionType::H2,
        ConnType::H3 => unimplemented!("H3 is not yet implemented"), // ConnectionType::H3,
    };

    let bytes = args.bytes.unwrap_or(10_000_000);

    println!("{}\n", args.url);

    let inflight_body = ThroughputClient::upload(bytes)
        .new_connection(conn_type)
        .send(
            args.url.parse()?,
            Arc::clone(&network),
            Arc::clone(&time),
            ShutdownSignal::from(&*shutdown_coordinator.handle()),
        )?
        .await?;

    let timing = inflight_body
        .timing
        .context("expected inflight body to have connection timing data")?;

    let body_start = time.now();
    let finished_result = wait_for_finish(inflight_body.events).await?;
    let finished = time.now();

    let time_body = finished.duration_since(body_start);
    let time_total = timing.time_secure() + time_body;

    println!(" time_lookup: {:.4}", timing.time_lookup().as_secs_f32());
    println!("time_connect: {:.4}", timing.time_connect().as_secs_f32());
    println!(" time_secure: {:.4}", timing.time_secure().as_secs_f32());
    println!(
        "   time_body: {:.4}",
        finished.duration_since(body_start).as_secs_f32()
    );
    println!("  time_total: {:.4}", time_total.as_secs_f32());
    println!();
    println!(" bytes_total: {}", finished_result.total);
    println!(
        "         bps: {:.4}",
        (finished_result.total * 8) as f32 / time_total.as_secs_f32()
    );

    let _ = shutdown_coordinator.shutdown_with_timeout(1).await;

    Ok(())
}
