use std::{future::Future, pin::Pin};

use nq_io::Network;
use tokio::sync::oneshot;

pub trait Speedtest<N: Network> {
    type TestResult;
    // type TestUpdates;

    fn run(
        self,
        network: N,
        shutdown: oneshot::Receiver<()>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Self::TestResult>> + Send + 'static>>;

    // fn updates(&self) -> mpsc::Receiver<Self::TestUpdates>;
}
