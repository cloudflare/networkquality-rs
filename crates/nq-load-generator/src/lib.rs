// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use http::{HeaderMap, HeaderName, HeaderValue};
use nq_core::client::{Direction, ThroughputClient};
use nq_core::{
    BodyEvent, ConnectionType, EstablishedConnection, Network, OneshotResult, ScopedHeaders, Time,
    Timestamp, oneshot_result,
};
use nq_stats::CounterSeries;
use rand::seq::SliceRandom;
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

#[derive(Debug, Deserialize)]
pub struct LoadConfig {
    pub headers: HashMap<String, String>,
    /// Headers attached only to requests whose host matches the scope's
    /// allowlist.
    #[serde(skip)]
    pub scoped_headers: Option<ScopedHeaders>,
    pub download_url: url::Url,
    pub upload_url: url::Url,
    pub upload_size: usize,
}

pub struct LoadGenerator {
    headers: HeaderMap<HeaderValue>,
    scoped_headers: Option<ScopedHeaders>,
    config: LoadConfig,
    loads: Vec<LoadedConnection>,
}

impl LoadGenerator {
    pub fn new(config: LoadConfig) -> anyhow::Result<Self> {
        let mut headers = HeaderMap::new();

        for (key, value) in config.headers.iter() {
            headers.insert(
                HeaderName::from_bytes(key.as_bytes())?,
                HeaderValue::from_bytes(value.as_bytes())?,
            );
        }

        Ok(Self {
            headers,
            scoped_headers: config.scoped_headers.clone(),
            config,
            loads: Vec::new(),
        })
    }

    #[tracing::instrument(skip(self, network, time, shutdown))]
    pub fn new_loaded_connection(
        &self,
        direction: Direction,
        conn_type: ConnectionType,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<OneshotResult<LoadedConnection>> {
        let (tx, rx) = oneshot_result();

        let client = match direction {
            Direction::Down => ThroughputClient::download(),
            Direction::Up(size) => ThroughputClient::upload(size),
        };

        let client = client
            .new_connection(conn_type)
            .headers(self.headers.clone())
            .scoped_headers(self.scoped_headers.clone());

        let response_fut = client.send(
            match direction {
                Direction::Up(_) => self.config.upload_url.as_str().parse()?,
                Direction::Down => self.config.download_url.as_str().parse()?,
            },
            network,
            time,
            shutdown,
        )?;

        tracing::debug!("got loaded connection response future");

        tokio::spawn(
            async move {
                let inflight_body = response_fut
                    .await
                    .context("could not await response for loaded connection")?;

                tracing::debug!("sending loaded connection");

                let _ = tx.send(Ok(LoadedConnection {
                    connection: inflight_body.connection,
                    events_rx: inflight_body.events,
                    state: LoadState::default(),
                }));

                Ok::<_, anyhow::Error>(())
            }
            .in_current_span(),
        );

        Ok(rx)
    }

    pub fn connections(&self) -> impl Iterator<Item = &LoadedConnection> {
        self.loads.iter()
    }

    pub fn random_connection(&self) -> Option<Arc<RwLock<EstablishedConnection>>> {
        let loads: Vec<_> = self.ongoing_loads().collect();
        loads
            .choose(&mut rand::thread_rng())
            .map(|c| c.connection.clone())
    }

    pub fn push(&mut self, loaded_connection: LoadedConnection) {
        self.loads.push(loaded_connection);
    }

    pub fn update(&mut self) {
        for load in &mut self.loads {
            load.update();
        }
    }

    /// Connections still transferring: neither completed nor terminated early.
    ///
    /// Excluding failed connections is what lets the ramp replace them and
    /// keeps self probes off dead connections.
    pub fn ongoing_loads(&self) -> impl Iterator<Item = &LoadedConnection> {
        self.loads.iter().filter(|load| load.is_ongoing())
    }

    pub fn count_loads(&self) -> usize {
        self.ongoing_loads().count()
    }

    /// Number of load-generating connections that terminated early with an
    /// error.
    pub fn count_failed_loads(&self) -> usize {
        self.loads.iter().filter(|load| load.has_failed()).count()
    }

    pub fn into_connections(self) -> Vec<LoadedConnection> {
        self.loads
    }
}

/// The observable state of a load-generating transfer.
///
/// Split out from [`LoadedConnection`] so the termination logic can be tested
/// without constructing a real connection.
#[derive(Debug, Default)]
struct LoadState {
    total_bytes_series: CounterSeries,
    finished_at: Option<Timestamp>,
    /// Set when the body's event channel closed *without* a `Finished` event,
    /// i.e. the transfer died mid-flight.
    failed: bool,
    /// Why the transfer failed, when a `Failed` event supplied a reason. A
    /// bare channel closure gives no reason, so this stays `None`.
    failure_reason: Option<String>,
    /// Set by [`LoadedConnection::stop`] so the channel closure it causes is
    /// not misreported as a failure.
    stopping: bool,
}

impl LoadState {
    fn apply(&mut self, event: BodyEvent) {
        match event {
            BodyEvent::ByteCount { at, total } => self.total_bytes_series.add(at, total as f64),
            BodyEvent::Finished { at } => self.finished_at = Some(at),
            BodyEvent::Failed { reason, .. } => {
                self.failed = true;
                self.failure_reason = Some(reason);
            }
        }
    }

    /// Handle the body's event channel closing.
    ///
    /// `CountingBody` owns the only sender, so a closed channel means the body
    /// was dropped. If that happened before a `Finished` event — and we are not
    /// deliberately tearing the load down — the transfer terminated early
    /// (stream reset, connection error, rejected request, ...).
    ///
    /// Without this, such a connection keeps `finished_at == None` forever and
    /// lingers in `ongoing_loads()` as a zombie: it contributes no further
    /// bytes to goodput yet still occupies a slot in the connection ramp.
    fn on_disconnected(&mut self) {
        if self.finished_at.is_none() && !self.stopping {
            self.failed = true;
        }
    }

    /// Whether the transfer is still running (neither completed nor failed).
    ///
    /// Note an upload load body is 32 GiB and so never completes within a test
    /// run; `finished_at == None` is therefore the normal healthy state for
    /// uploads, which is exactly why a failure needs its own signal.
    fn is_ongoing(&self) -> bool {
        self.finished_at.is_none() && !self.failed
    }

    /// Drain all currently-available body events, and notice if the channel has
    /// closed.
    ///
    /// `try_recv` yields any buffered events before reporting `Disconnected`,
    /// so a body that emitted `Finished` and was then dropped is correctly seen
    /// as completed rather than failed.
    fn drain(&mut self, events_rx: &mut UnboundedReceiver<BodyEvent>) {
        loop {
            match events_rx.try_recv() {
                Ok(event) => self.apply(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.on_disconnected();
                    break;
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct LoadedConnection {
    connection: Arc<RwLock<EstablishedConnection>>,
    events_rx: UnboundedReceiver<BodyEvent>,
    state: LoadState,
}

impl LoadedConnection {
    pub fn update(&mut self) {
        self.state.drain(&mut self.events_rx);
    }

    pub fn total_bytes_series(&self) -> &CounterSeries {
        &self.state.total_bytes_series
    }

    /// Whether this connection is still transferring.
    pub fn is_ongoing(&self) -> bool {
        self.state.is_ongoing()
    }

    /// Whether this connection terminated early with an error.
    pub fn has_failed(&self) -> bool {
        self.state.failed
    }

    /// Why this connection failed, if a reason was reported.
    pub fn failure_reason(&self) -> Option<&str> {
        self.state.failure_reason.as_deref()
    }

    pub fn stop(&mut self) {
        self.state.stopping = true;
        self.events_rx.close();
        self.update();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn channel() -> (
        mpsc::UnboundedSender<BodyEvent>,
        mpsc::UnboundedReceiver<BodyEvent>,
    ) {
        mpsc::unbounded_channel()
    }

    #[test]
    fn open_channel_leaves_transfer_ongoing() {
        let (tx, mut rx) = channel();
        tx.send(BodyEvent::ByteCount {
            at: Timestamp::now(),
            total: 1024,
        })
        .unwrap();

        let mut state = LoadState::default();
        state.drain(&mut rx);

        assert!(state.is_ongoing());
        assert!(!state.failed);
        // Keep the sender alive: an open channel must not look like a failure.
        drop(tx);
    }

    #[test]
    fn disconnect_without_finished_marks_failed() {
        let (tx, mut rx) = channel();
        tx.send(BodyEvent::ByteCount {
            at: Timestamp::now(),
            total: 10 * 1024 * 1024,
        })
        .unwrap();
        // The body was dropped mid-transfer (e.g. the server rejected the
        // upload with 413), closing the channel without a `Finished` event.
        drop(tx);

        let mut state = LoadState::default();
        state.drain(&mut rx);

        assert!(state.failed, "early termination must be flagged");
        assert!(!state.is_ongoing(), "a failed load must not stay ongoing");
    }

    #[test]
    fn finished_then_disconnect_is_not_a_failure() {
        let (tx, mut rx) = channel();
        let at = Timestamp::now();
        tx.send(BodyEvent::ByteCount { at, total: 512 }).unwrap();
        tx.send(BodyEvent::Finished { at }).unwrap();
        // Normal completion: the body is dropped right after finishing. The
        // buffered events must be drained before `Disconnected` is observed.
        drop(tx);

        let mut state = LoadState::default();
        state.drain(&mut rx);

        assert!(!state.failed, "a completed transfer must not be a failure");
        assert_eq!(state.finished_at, Some(at));
        assert!(!state.is_ongoing(), "a completed load is no longer ongoing");
    }

    #[test]
    fn teardown_disconnect_is_not_a_failure() {
        // `stop()` closes the receiver itself; that must not be mistaken for
        // the connection dying, otherwise every run would end "with failures".
        let (tx, mut rx) = channel();
        drop(tx);

        let mut state = LoadState::default();
        state.stopping = true;
        state.drain(&mut rx);

        assert!(!state.failed, "teardown must not be flagged as a failure");
    }

    #[test]
    fn explicit_failed_event_retires_the_load_with_a_reason() {
        // e.g. an upload rejected with 413: the client reports the failure the
        // body itself cannot see.
        let (tx, mut rx) = channel();
        let at = Timestamp::now();
        tx.send(BodyEvent::ByteCount { at, total: 1024 }).unwrap();
        tx.send(BodyEvent::Failed {
            at,
            reason: "upload rejected with status 413 Payload Too Large".to_owned(),
        })
        .unwrap();

        let mut state = LoadState::default();
        state.drain(&mut rx);

        assert!(state.failed);
        assert!(!state.is_ongoing());
        assert_eq!(
            state.failure_reason.as_deref(),
            Some("upload rejected with status 413 Payload Too Large")
        );
        // Sender still alive: the failure must be recognised from the event
        // alone, without relying on the channel closing.
        drop(tx);
    }

    #[test]
    fn bytes_seen_before_failure_are_retained() {
        // A failed connection still transferred real bytes; goodput accounting
        // must keep them.
        let (tx, mut rx) = channel();
        let at = Timestamp::now();
        tx.send(BodyEvent::ByteCount { at, total: 4096 }).unwrap();
        drop(tx);

        let mut state = LoadState::default();
        state.drain(&mut rx);

        assert!(state.failed);
        assert_eq!(state.total_bytes_series.sum(), 4096.0);
    }
}
