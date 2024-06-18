mod http;
mod map;

use std::time::Duration;

use crate::Timestamp;

pub use self::map::ConnectionManager;
pub use self::http::EstablishedConnection;


/// The L7 type of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    /// Create an HTTP/1.1 connection.
    H1,
    /// Create an HTTP/2 connection.
    H2,
    /// Create an HTTP/3 connection.
    H3,
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
}
