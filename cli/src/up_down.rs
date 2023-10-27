use std::sync::Arc;

use anyhow::Context;
use nq_core::client::{wait_for_finish, ThroughputClient};
use nq_core::{ConnectionType, Network, Time};
use tokio::sync::oneshot;

use crate::args::up_down::DownloadArgs;
use crate::args::ConnType;

/// Run a download test.
pub async fn download(
    args: DownloadArgs,
    network: Arc<dyn Network>,
    time: Arc<dyn Time>,
    _shutdown: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let conn_type = match args.conn_type {
        ConnType::H1 => unimplemented!("H1 is not yet implemented"), // ConnectionType::H1,
        ConnType::H2 => ConnectionType::H2,
        ConnType::H3 => unimplemented!("H3 is not yet implemented"), // ConnectionType::H3,
    };

    let start = time.now();
    let inflight_body = ThroughputClient::download()
        .new_connection(conn_type)
        .send(args.url.parse()?, Arc::clone(&network), Arc::clone(&time))?
        .await?;

    let timing = inflight_body
        .timing
        .context("expected inflight body to have connection timing data")?;

    let finished_result = wait_for_finish(inflight_body.events).await?;
    let duration = time.now().duration_since(start);

    println!(" time_lookup: {:.4}", timing.time_lookup().as_secs_f32());
    println!("time_connect: {:.4}", timing.time_connect().as_secs_f32());
    println!(" time_secure: {:.4}", timing.time_secure().as_secs_f32());
    println!("    duration: {:.4}", duration.as_secs_f32());
    println!(
        "         bps: {:.4}",
        (finished_result.total * 8) as f32 / duration.as_secs_f32()
    );
    println!(" bytes_total: {}", finished_result.total);

    Ok(())
}
