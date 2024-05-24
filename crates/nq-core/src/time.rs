//! Defines a [`Time`] trait used to abstract over the different ways a
//! timestamp can be created or a process slept. This lets us switch between
//! tokio, system and in the future, wasi/wasm based time implementations.

use std::ops::{Add, Sub};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

/// A timestamp with `Instant` for precise time measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(Instant);

impl Timestamp {
    /// Calculate the saturating duration since an earlier timestamp.
    pub fn duration_since(&self, earlier: Timestamp) -> Duration {
        self.0.checked_duration_since(earlier.0).unwrap_or_else(|| Duration::from_secs(0))
    }

    /// Calculate the duration elapsed since the creation of this timestamp.
    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    /// Create a new `Timestamp` from the current `Instant`.
    pub fn now() -> Self {
        Timestamp(Instant::now())
    }

    /// Create a new `Timestamp` from the current `Instant` relative to a base `Instant`.
    pub fn now_instant(base_instant: Instant) -> Self {
        let now = Instant::now();
        let duration = now.duration_since(base_instant);
        Timestamp(base_instant + duration)
    }

    /// Create a new `Timestamp` from a relative duration in microseconds
    pub fn from_duration_micros(micros: u64) -> Self {
        Timestamp(Instant::now() + Duration::from_micros(micros))
    }
}

impl Add<Duration> for Timestamp {
    type Output = Timestamp;

    fn add(self, duration: Duration) -> Self::Output {
        Timestamp(self.0 + duration)
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Timestamp;

    fn sub(self, duration: Duration) -> Self::Output {
        Timestamp(self.0 - duration)
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

/// An implementation of `Time` based on `tokio::Instant`.
#[derive(Debug, Clone, Copy)]
pub struct TokioTime {
    base_instant: Instant,
    base_timestamp: Timestamp,
}

impl TokioTime {
    /// Creates a new `TokioTime`.
    pub fn new() -> Self {
        let base_instant = tokio::time::Instant::now();
        let base_timestamp = Timestamp::now(); // Use the current timestamp

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
        let now = Instant::now();
        let elapsed = now.duration_since(self.base_instant);
        self.base_timestamp + elapsed
    }
}
