// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

mod http;
mod map;

use std::time::Duration;

use crate::Timestamp;

pub use self::http::{EstablishedConnection, insecure_tls, set_insecure_tls};
pub use self::map::ConnectionManager;

/// The L7 type of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    /// Create an HTTP/1.1 connection. To disable tls, set `use_tls: false`.
    H1 {
        /// enable tls for this HTTP/1.1 connection.
        use_tls: bool,
    },
    /// Create an HTTP/2 connection.
    H2,
    /// Create an HTTP/3 connection.
    H3,
}

impl ConnectionType {
    /// Creates an HTTP/1.1 connection type with TLS disabled.
    pub fn h1_clear_text() -> ConnectionType {
        ConnectionType::H1 { use_tls: false }
    }

    /// Creates an HTTP/1.1 connection type.
    pub fn h1() -> ConnectionType {
        ConnectionType::H1 { use_tls: true }
    }

    /// Creates an HTTP/2 connection type.
    pub fn h2() -> ConnectionType {
        ConnectionType::H2
    }

    /// Creates an HTTP/3 connection type.
    pub fn h3() -> ConnectionType {
        ConnectionType::H3
    }
}

/// Timing stats for the establishment of a connection. All durations
/// are calculated from the start of the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConnectionTiming {
    /// When the connection was started.
    start: Timestamp,
    /// How long it took to resolve the host to an IP.
    time_lookup: Duration,
    /// How long it took for the transport to handshake.
    ///
    /// If this was a TCP connection, this is the time
    /// until the first SYN+ACK.
    ///
    /// If this is a QUIC connection, this is the time until
    /// the QUIC handshake completes.
    time_connect: Duration,
    /// How long it took to secure the stream after the transport
    /// connected.
    ///
    /// For TCP streams, this is the time to perform the TLS handshake.
    ///
    /// For QUIC streams, this is 0, since the QUIC connection implies
    /// a secured connection.
    time_secure: Duration,
    /// How long it took to setup the L7 protocol, H1/2/3.
    time_application: Duration,

    // Duration of the DNS lookup
    dns_time: Duration,

    /// Number of round-trips the TLS handshake took until the connection was
    /// ready to transmit data (TLS 1.3 -> 1, TLS 1.2 -> 2). Defaults to 1.
    ///
    /// Used to normalize the TLS handshake time per draft-ietf-ippm-
    /// responsiveness-09 §5.3 ("the TLS establishment time needs to be
    /// normalized to the number of round-trips").
    tls_round_trips: u32,
}

impl ConnectionTiming {
    /// Creates a new [`ConnectionTiming`].
    pub fn new(start: Timestamp) -> Self {
        Self {
            start,
            time_lookup: Duration::ZERO,
            time_connect: Duration::ZERO,
            time_secure: Duration::ZERO,
            time_application: Duration::ZERO,
            dns_time: Duration::ZERO,
            tls_round_trips: 1,
        }
    }

    /// Set the time it took to perform DNS resolution of the peer's host.
    pub fn set_lookup(&mut self, at: Timestamp) {
        self.time_lookup = at.duration_since(self.start);
    }

    /// Set the time it took to create the connection with the remote peer.
    pub fn set_connect(&mut self, at: Timestamp) {
        self.time_connect = at.duration_since(self.start);
    }

    /// Set the time it took to secure a connection.
    pub fn set_secure(&mut self, at: Timestamp) {
        self.time_secure = at.duration_since(self.start);
    }

    /// Set the time it took to setup the L7 protocol, H1/2/3.
    pub fn set_application(&mut self, at: Timestamp) {
        self.time_application = at.duration_since(self.start);
    }

    /// Returns when the connection started.
    pub fn start(&self) -> Timestamp {
        self.start
    }

    /// Returns how long it took for DNS to resolve.
    pub fn time_lookup(&self) -> Duration {
        self.time_lookup
    }

    /// Returns how long it took for the transport to connect.
    pub fn time_connect(&self) -> Duration {
        self.time_connect
    }

    /// Set the duration of the DNS lookup
    pub fn set_dns_lookup(&mut self, duration: Duration) {
        self.dns_time = duration;
    }

    /// Returns the DNS lookup duration.
    pub fn dns_time(&self) -> Duration {
        self.dns_time
    }

    /// Returns how long it took for the security handshake to complete.
    pub fn time_secure(&self) -> Duration {
        self.time_secure
    }

    /// Returns how long it took for the H/{1,2,3} handshake to complete.
    pub fn time_application(&self) -> Duration {
        self.time_application
    }

    /// Sets the number of round-trips the TLS handshake took.
    pub fn set_tls_round_trips(&mut self, round_trips: u32) {
        self.tls_round_trips = round_trips.max(1);
    }

    /// Returns the number of round-trips the TLS handshake took (>= 1).
    pub fn tls_round_trips(&self) -> u32 {
        self.tls_round_trips.max(1)
    }

    /// The duration of the TCP handshake alone (excluding DNS resolution),
    /// i.e. `tcp_f` in draft-ietf-ippm-responsiveness-09 §5.3.
    ///
    /// This is the interval between the transport starting to connect and the
    /// connection being established. When the connection timing starts after
    /// DNS resolution (as it does for the responsiveness probes), `time_lookup`
    /// is zero and this is simply `time_connect`.
    pub fn tcp_handshake(&self) -> Duration {
        self.time_connect.saturating_sub(self.time_lookup)
    }

    /// The duration of the TLS handshake alone (excluding the preceding TCP
    /// handshake), i.e. the un-normalized `tls_f` in
    /// draft-ietf-ippm-responsiveness-09 §5.3.
    ///
    /// For QUIC/H3 connections `time_secure` is zero (TLS is folded into the
    /// transport handshake), so this saturates to zero rather than underflowing.
    /// Divide by [`Self::tls_round_trips`] to obtain the normalized value.
    pub fn tls_handshake(&self) -> Duration {
        self.time_secure.saturating_sub(self.time_connect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a timing whose phases complete at the given millisecond offsets
    /// from `start`.
    fn timing_at(lookup_ms: u64, connect_ms: u64, secure_ms: u64, application_ms: u64) -> ConnectionTiming {
        let start = Timestamp::now();
        let mut t = ConnectionTiming::new(start);
        t.set_lookup(start + Duration::from_millis(lookup_ms));
        t.set_connect(start + Duration::from_millis(connect_ms));
        t.set_secure(start + Duration::from_millis(secure_ms));
        t.set_application(start + Duration::from_millis(application_ms));
        t
    }

    #[test]
    fn independent_phase_deltas() {
        // Post-DNS baseline (lookup = 0): connect at 30ms, secure at 60ms,
        // application at 62ms. Each network phase is ~30ms (1 RTT).
        let t = timing_at(0, 30, 60, 62);
        assert_eq!(t.tcp_handshake(), Duration::from_millis(30));
        assert_eq!(t.tls_handshake(), Duration::from_millis(30));
    }

    #[test]
    fn tcp_handshake_excludes_dns_lookup() {
        // If the timing baseline included DNS (lookup at 10ms, connect at 40ms),
        // the TCP handshake is connect - lookup = 30ms, not 40ms.
        let t = timing_at(10, 40, 70, 72);
        assert_eq!(t.tcp_handshake(), Duration::from_millis(30));
        assert_eq!(t.tls_handshake(), Duration::from_millis(30));
    }

    #[test]
    fn tls_handshake_saturates_for_quic_like_zero_secure() {
        // QUIC/H3: time_secure stays 0 while time_connect is set. Must not
        // underflow.
        let start = Timestamp::now();
        let mut t = ConnectionTiming::new(start);
        t.set_connect(start + Duration::from_millis(30));
        // secure left at zero
        assert_eq!(t.tls_handshake(), Duration::ZERO);
    }

    #[test]
    fn tls_round_trips_defaults_to_one_and_clamps() {
        let t = ConnectionTiming::new(Timestamp::now());
        assert_eq!(t.tls_round_trips(), 1);

        let mut t = t;
        t.set_tls_round_trips(2);
        assert_eq!(t.tls_round_trips(), 2);

        // A zero round-trip count would produce a divide-by-zero downstream;
        // it is clamped to 1.
        t.set_tls_round_trips(0);
        assert_eq!(t.tls_round_trips(), 1);
    }
}
