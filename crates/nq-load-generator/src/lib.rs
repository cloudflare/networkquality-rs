// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use nq_core::client::{Direction, ThroughputClient};
use nq_core::{
    BodyEvent, ConnectionType, EstablishedConnection, InflightBody, Network, OneshotResult,
    ScopedHeaders, Time, Timestamp, oneshot_result,
};
use nq_stats::CounterSeries;
use rand::seq::SliceRandom;
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
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

        let uri: Uri = match direction {
            Direction::Up(_) => self.config.upload_url.as_str().parse()?,
            Direction::Down => self.config.download_url.as_str().parse()?,
        };

        let client = match direction {
            Direction::Down => ThroughputClient::download(),
            Direction::Up(size) => ThroughputClient::upload(size),
        };

        let client = client
            .new_connection(conn_type)
            .headers(self.headers.clone())
            .scoped_headers(self.scoped_headers.clone());

        let response_fut = client.send(
            uri.clone(),
            Arc::clone(&network),
            Arc::clone(&time),
            shutdown.clone(),
        )?;

        tracing::debug!("got loaded connection response future");

        // An upload load is an open-ended *sequence* of bounded requests rather
        // than one request, so its events are produced by a driver task instead
        // of coming straight off a single body. See [`UploadReissue`].
        let reissue = match direction {
            Direction::Up(bound) => Some(UploadReissue {
                bound,
                uri,
                headers: self.headers.clone(),
                scoped_headers: self.scoped_headers.clone(),
                network,
                time,
                shutdown,
            }),
            Direction::Down => None,
        };

        tokio::spawn(
            async move {
                let inflight_body = response_fut
                    .await
                    .context("could not await response for loaded connection")?;

                tracing::debug!("sending loaded connection");

                let Some(reissue) = reissue else {
                    let _ = tx.send(Ok(LoadedConnection {
                        connection: inflight_body.connection,
                        events_rx: inflight_body.events,
                        state: LoadState::default(),
                    }));

                    return Ok(());
                };

                let (events_tx, events_rx) = mpsc::unbounded_channel();
                let connection = Arc::clone(&inflight_body.connection);

                let _ = tx.send(Ok(LoadedConnection {
                    connection: Arc::clone(&connection),
                    events_rx,
                    state: LoadState::default(),
                }));

                reissue
                    .run(connection, inflight_body.events, events_tx)
                    .await;

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

/// Drives an upload load-generating connection as an open-ended sequence of
/// bounded POSTs, all sent on the same established connection.
///
/// A single unbounded POST cannot be used against a server that caps how much
/// request body it will buffer. Cloudflare's edge rejects one with HTTP 413 at
/// 500 MB (RADAR-7233), which kills the load part-way through the test; the RPM
/// score then reflects a network that is barely loaded, so it comes out
/// flatteringly high rather than simply failing.
///
/// Two measured properties of that cap make this approach work: it applies
/// per-request rather than per-connection, and a 413 does not close the HTTP/2
/// connection. So an unbounded number of bounded requests can ride one
/// connection and keep the link continuously loaded without tripping it.
struct UploadReissue {
    /// Maximum bytes sent in any single request.
    bound: usize,
    uri: Uri,
    headers: HeaderMap<HeaderValue>,
    scoped_headers: Option<ScopedHeaders>,
    network: Arc<dyn Network>,
    time: Arc<dyn Time>,
    shutdown: CancellationToken,
}

/// Why the request currently being relayed stopped producing events.
#[derive(Debug, PartialEq, Eq)]
enum RequestEnd {
    /// The body sent every byte it was asked for.
    Finished,
    /// The channel closed before the body finished, i.e. the transfer died.
    Died,
}

impl UploadReissue {
    /// Relay `first`'s events, then keep issuing further bounded requests on
    /// `connection` for as long as the consumer keeps listening.
    async fn run(
        self,
        connection: Arc<RwLock<EstablishedConnection>>,
        first: UnboundedReceiver<BodyEvent>,
        events_tx: mpsc::UnboundedSender<BodyEvent>,
    ) {
        let mut current = first;
        let mut relay = CumulativeRelay::default();
        let mut requests = 1usize;

        loop {
            let ended = loop {
                let event = tokio::select! {
                    // Test teardown. Returning silently is correct: the consumer
                    // tells teardown apart from a failure via
                    // `LoadedConnection::stop`, which sets `stopping` before it
                    // observes the channel closing.
                    _ = self.shutdown.cancelled() => return,
                    event = current.recv() => event,
                };

                let Some(event) = event else {
                    break RequestEnd::Died;
                };

                match relay.on_event(event) {
                    RelayAction::Forward(event) => {
                        // A closed channel means `stop()` was called. Returning
                        // drops `current`, closing the in-flight body's event
                        // channel, which is what truncates it -- the same
                        // mechanism a single-request load uses.
                        if events_tx.send(event).is_err() {
                            return;
                        }
                    }
                    RelayAction::RequestFinished => break RequestEnd::Finished,
                    RelayAction::Fail(event) => {
                        let _ = events_tx.send(event);
                        return;
                    }
                }
            };

            if ended == RequestEnd::Died {
                let _ = events_tx.send(BodyEvent::Failed {
                    at: self.time.now(),
                    reason: format!(
                        "upload terminated early after {} request(s), {} bytes",
                        requests,
                        relay.total()
                    ),
                });
                return;
            }

            if events_tx.is_closed() {
                return;
            }

            // Start the replacement before dealing with the finished request, so
            // the connection is refilled as early as possible. `Finished` fires
            // when the body hands its last frame to hyper, which still has that
            // data buffered -- so the new request's frames queue behind the tail
            // of the old one and the socket never goes idle.
            let next = match self.issue(&connection) {
                Ok(next) => next,
                Err(error) => {
                    let _ = events_tx.send(BodyEvent::Failed {
                        at: self.time.now(),
                        reason: format!("could not start upload request {requests}: {error:#}"),
                    });
                    return;
                }
            };

            let next = match next.await {
                Ok(inflight) => inflight.events,
                Err(error) => {
                    let _ = events_tx.send(BodyEvent::Failed {
                        at: self.time.now(),
                        reason: format!("upload request {requests} failed to start: {error:#}"),
                    });
                    return;
                }
            };

            requests += 1;
            tracing::debug!(
                requests,
                total_bytes = relay.total(),
                "re-issued bounded upload request"
            );

            // Because `Finished` precedes the response, the status of the
            // request just completed is still unknown. Keep draining its channel
            // in the background so a late rejection still retires this load.
            let finished = std::mem::replace(&mut current, next);
            tokio::spawn(watch_tail(finished, events_tx.clone()).in_current_span());
        }
    }

    fn issue(
        &self,
        connection: &Arc<RwLock<EstablishedConnection>>,
    ) -> anyhow::Result<OneshotResult<InflightBody>> {
        ThroughputClient::upload(self.bound)
            .with_connection(Arc::clone(connection))
            .headers(self.headers.clone())
            .scoped_headers(self.scoped_headers.clone())
            .send(
                self.uri.clone(),
                Arc::clone(&self.network),
                Arc::clone(&self.time),
                self.shutdown.clone(),
            )
    }
}

/// Drain a completed request's event channel, forwarding only a terminal
/// failure.
///
/// [`UploadReissue::run`] moves to the next request as soon as the previous body
/// is fully handed to hyper, which happens before its response status is known.
/// A rejection therefore arrives after the driver has stopped reading that
/// channel; without this it would be dropped, leaving the load looking healthy
/// while the server refuses every request.
async fn watch_tail(
    mut events: UnboundedReceiver<BodyEvent>,
    events_tx: mpsc::UnboundedSender<BodyEvent>,
) {
    while let Some(event) = events.recv().await {
        if matches!(event, BodyEvent::Failed { .. }) {
            let _ = events_tx.send(event);
            return;
        }
    }
}

/// Translates the per-request [`BodyEvent`] streams of a re-issued upload into
/// one continuous stream for the consumer.
///
/// Every request's `CountingBody` counts from zero, but [`CounterSeries`] treats
/// its samples as a cumulative counter and derives goodput from `end - start`.
/// Forwarding a per-request total would make that difference *negative* at every
/// request boundary, silently corrupting goodput and the saturation detection
/// built on top of it. Totals are therefore rebased onto a running sum here.
#[derive(Debug, Default)]
struct CumulativeRelay {
    /// Bytes accounted for by requests that have already completed.
    base: usize,
    /// Most recent total reported by the in-flight request.
    last: usize,
}

/// What [`UploadReissue::run`] should do with a translated event.
#[derive(Debug)]
enum RelayAction {
    /// Pass this event on to the consumer.
    Forward(BodyEvent),
    /// The current request completed; start another. Deliberately forwards
    /// nothing: a `Finished` would set `finished_at` and retire a load that is
    /// in fact still running.
    RequestFinished,
    /// Terminal failure. Forward it and stop.
    Fail(BodyEvent),
}

impl CumulativeRelay {
    fn on_event(&mut self, event: BodyEvent) -> RelayAction {
        match event {
            BodyEvent::ByteCount { at, total } => {
                self.last = total;
                RelayAction::Forward(BodyEvent::ByteCount {
                    at,
                    total: self.base + total,
                })
            }
            BodyEvent::Finished { .. } => {
                self.base += self.last;
                self.last = 0;
                RelayAction::RequestFinished
            }
            BodyEvent::Failed { at, reason } => RelayAction::Fail(BodyEvent::Failed { at, reason }),
        }
    }

    /// Total bytes sent across every request so far.
    fn total(&self) -> usize {
        self.base + self.last
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
    /// `finished_at == None` is the normal healthy state for an upload:
    /// [`UploadReissue`] replaces each bounded request as it completes and
    /// swallows the per-request `Finished`, so an upload load never reports
    /// completion. That is exactly why a failure needs its own signal.
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
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn channel() -> (
        mpsc::UnboundedSender<BodyEvent>,
        mpsc::UnboundedReceiver<BodyEvent>,
    ) {
        mpsc::unbounded_channel()
    }

    /// Feed a `ByteCount` through the relay and return the total it forwarded.
    fn forward_bytes(relay: &mut CumulativeRelay, at: Timestamp, total: usize) -> usize {
        match relay.on_event(BodyEvent::ByteCount { at, total }) {
            RelayAction::Forward(BodyEvent::ByteCount { total, .. }) => total,
            other => panic!("a ByteCount must be forwarded, got {other:?}"),
        }
    }

    #[test]
    fn totals_accumulate_across_request_boundaries() {
        let at = Timestamp::now();
        let mut relay = CumulativeRelay::default();

        assert_eq!(forward_bytes(&mut relay, at, 40), 40);
        assert_eq!(forward_bytes(&mut relay, at, 100), 100);
        relay.on_event(BodyEvent::Finished { at });

        // The next request counts from zero again; the consumer must not see
        // that reset.
        assert_eq!(forward_bytes(&mut relay, at, 0), 100);
        assert_eq!(forward_bytes(&mut relay, at, 30), 130);
        relay.on_event(BodyEvent::Finished { at });

        assert_eq!(forward_bytes(&mut relay, at, 5), 135);
        assert_eq!(relay.total(), 135);
    }

    #[test]
    fn request_finished_is_never_forwarded() {
        // A forwarded `Finished` would set `finished_at`, so `is_ongoing()` would
        // go false and the ramp would retire a connection that is still running.
        let at = Timestamp::now();
        let mut relay = CumulativeRelay::default();

        assert!(matches!(
            relay.on_event(BodyEvent::Finished { at }),
            RelayAction::RequestFinished
        ));
    }

    #[test]
    fn failure_is_terminal_and_forwarded() {
        let at = Timestamp::now();
        let mut relay = CumulativeRelay::default();

        let action = relay.on_event(BodyEvent::Failed {
            at,
            reason: "upload rejected with status 413 Payload Too Large".to_owned(),
        });

        match action {
            RelayAction::Fail(BodyEvent::Failed { reason, .. }) => {
                assert!(reason.contains("413"));
            }
            other => panic!("a Failed must be forwarded as terminal, got {other:?}"),
        }
    }

    // The regression that matters most. `CounterSeries::interval_sum` is
    // `end - start`, so if a request boundary ever let a total reset to zero
    // reach the series, goodput for that window would go *negative* -- which
    // would silently corrupt the saturation detection that decides when the
    // test has reached working conditions.
    #[test]
    fn relayed_totals_never_produce_negative_goodput() {
        let start = Timestamp::now();
        let step = Duration::from_millis(50);

        let mut relay = CumulativeRelay::default();
        let mut series = CounterSeries::new();
        let mut at = start;

        // Three consecutive 100-byte requests, each reporting in 25-byte steps.
        for _ in 0..3 {
            for total in [0usize, 25, 50, 75, 100] {
                at = at + step;
                let forwarded = forward_bytes(&mut relay, at, total);
                series.add(at, forwarded as f64);
            }
            at = at + step;
            relay.on_event(BodyEvent::Finished { at });
        }

        assert_eq!(relay.total(), 300, "three 100-byte requests");

        let mut window = start;
        while window < at {
            let next = window + step;
            let bytes = series.interval_sum(window, next);
            assert!(
                bytes >= 0.0,
                "negative goodput ({bytes}) in one window -- a request boundary leaked a reset"
            );
            window = next;
        }

        assert_eq!(
            series.interval_sum(start, at),
            300.0,
            "the whole run must account for every byte exactly once"
        );
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
