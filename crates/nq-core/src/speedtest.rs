//! A simple abstraction over a [`Speedtest`] and its test result.

use std::{future::Future, pin::Pin, sync::Arc};

use shellflip::ShutdownSignal;

use crate::{Network, Time};

/// A simple abstraction over the concept of a [`Speedtest`].
pub trait Speedtest {
    /// The result of the speedtest.
    type TestResult;

    /// Run the speedtest with the given network and time.
    fn run(
        self,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        shutdown: ShutdownSignal,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Self::TestResult>> + Send + 'static>>;
}
