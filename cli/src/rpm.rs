use std::sync::Arc;
use std::time::Duration;

use nq_core::{Network, Speedtest, Time};
use nq_rpm::{Responsiveness, ResponsivenessConfig};
use tokio::sync::oneshot;

use crate::args::rpm::RpmArgs;

pub async fn run(
    cli_config: RpmArgs,
    network: Arc<dyn Network>,
    time: Arc<dyn Time>,
    shutdown: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let config = ResponsivenessConfig {
        large_download_url: cli_config.large_download_url.parse()?,
        small_download_url: cli_config.small_download_url.parse()?,
        upload_url: cli_config.upload_url.parse()?,
        moving_average_distance: cli_config.moving_average_distance,
        interval_duration: Duration::from_millis(cli_config.interval_duration_ms),
        test_duration: Duration::from_millis(cli_config.test_duration_ms),
        trimmed_mean_percent: cli_config.trimmed_mean_percent,
        std_tolerance: cli_config.std_tolerance,
        max_loaded_connections: cli_config.max_loaded_connections,
    };

    let download_rpm = Responsiveness::new(config.clone(), true)?;
    let download_result = download_rpm
        .run(Arc::clone(&network), Arc::clone(&time), shutdown)
        .await?;

    println!("{}", download_result);

    // todo(fisher): need to properly shutdown connections before running the
    // upload test.

    // let (_tx, rx) = oneshot::channel();
    // let upload_rpm = Responsiveness::new(config, false)?;
    // let upload_result = upload_rpm.run(network, time, rx).await?;
    // println!("upload result: \n{:?}", upload_result);

    Ok(())
}
