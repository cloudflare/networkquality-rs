//! Defines a [`Time`] trait used to abstract over the different ways a
//! timestamp can be created or a process slept. This lets us switch between
//! tokio, system and in the future, wasi/wasm based time implementations.

use std::ops::{Add, Sub};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use tokio::time::Instant;

/// A UNIX epoch timestamp with microsecond granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(u64);

impl Timestamp {
    const EPOCH: Timestamp = Timestamp(0);

    /// Calculate the saturating duration since an earlier timestamp.
    pub fn duration_since(&self, earlier: Timestamp) -> Duration {
        Duration::from_micros(self.0.saturating_sub(earlier.0))
    }

    /// Calculate the duration elapsed since the creation of this timestamp.
    pub fn elapsed(&self, time: &dyn Time) -> Duration {
        self.duration_since(time.now())
    }

    /// Create a [`Timestamp`] from a [`SystemTime`].
    pub fn from_system_time(time: SystemTime) -> Self {
        let duration = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();

        Timestamp::from(duration.as_micros() as u64)
    }

    /// Create a [`Timestamp`] from [`SystemTime::now`].
    pub fn now_std() -> Self {
        Timestamp::from_system_time(SystemTime::now())
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::EPOCH
    }
}

impl From<u64> for Timestamp {
    fn from(timestamp: u64) -> Self {
        Timestamp(timestamp)
    }
}

impl Add<Duration> for Timestamp {
    type Output = Timestamp;

    fn add(self, duration: Duration) -> Self::Output {
        let us = u64::try_from(duration.as_micros()).expect("duration overflows u64");

        Timestamp(
            self.0
                .checked_add(us)
                .expect("overflow when adding duration from timestamp"),
        )
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Timestamp;

    fn sub(self, duration: Duration) -> Self::Output {
        let us = u64::try_from(duration.as_micros()).expect("duration overflows u64");

        Timestamp(
            self.0
                .checked_sub(us)
                .expect("overflow when subtracting duration from timestamp"),
        )
    }
}

/// An abstraction over time. Provides the ability to create a timestamp.
pub trait Time: Send + Sync {
    /// The current time.
    fn now(&self) -> Timestamp;
}

impl<T: Time> Time for Arc<T>
where
    T: Time,
{
    fn now(&self) -> Timestamp {
        <T as Time>::now(self)
    }
}

impl<T: Time> Time for Box<T>
where
    T: Time,
{
    fn now(&self) -> Timestamp {
        <T as Time>::now(self)
    }
}

impl<T: Time> Time for &T
where
    T: Time,
{
    fn now(&self) -> Timestamp {
        <T as Time>::now(self)
    }
}

/// An implementation of [`Time`] based off of tokio.
#[derive(Debug, Clone, Copy)]
pub struct TokioTime {
    base_instant: Instant,
    base_timestamp: Timestamp,
}

impl TokioTime {
    /// Creates a new [`TokioTime`].
    pub fn new() -> Self {
        let base_instant = tokio::time::Instant::now();
        let base_timestamp = Timestamp::from_system_time(SystemTime::now());

        Self {
            base_instant,
            base_timestamp,
        }
    }
}

impl Default for TokioTime {
    fn default() -> Self {
        Self::new()
    }
}

impl Time for TokioTime {
    fn now(&self) -> Timestamp {
        let now = tokio::time::Instant::now();

        let elapsed = now
            .checked_duration_since(self.base_instant)
            .expect("tokio::time::Instant went back in time");

        self.base_timestamp + elapsed
    }
}

/// An implementation of [`Time`] based off of the rust standard library. For
/// now, it still uses tokio for sleep purposes.
pub struct StdTime;

impl Time for StdTime {
    fn now(&self) -> Timestamp {
        Timestamp::from_system_time(SystemTime::now())
    }
}
