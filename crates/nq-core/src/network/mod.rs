//! Defines an abstraction over networks in order to support multiple different
//! connection types and underlying IOs.

use std::net::SocketAddr;
use std::sync::Arc;

use http::Response;
use hyper::body::Incoming;

use crate::{body::NqBody, connection::NewConnection, OneshotResult, Timestamp};

pub use crate::connection::{ConnectionId, ConnectionTiming, ConnectionType};

/// A network abstraction. Allows us to easily stack [`Network`]s on top of each
/// other, to easily create multi-hop networks over different types of
/// connections.
pub trait Network: Send + Sync + 'static {
    /// Resolve a host to a list of [`SocketAddr`]s.
    fn resolve(&self, host: String) -> OneshotResult<Vec<SocketAddr>>;

    /// Create a new connection to the given domain.
    fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> OneshotResult<NewConnection>;

    /// Send a request over the given connection.
    fn send_request(
        &self,
        conn_id: ConnectionId,
        request: http::Request<NqBody>,
    ) -> OneshotResult<Response<Incoming>>;
}

impl Network for Arc<dyn Network> {
    fn resolve(&self, host: String) -> OneshotResult<Vec<SocketAddr>> {
        self.as_ref().resolve(host)
    }

    fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> OneshotResult<NewConnection> {
        self.as_ref()
            .new_connection(start, remote_addr, domain, conn_type)
    }

    fn send_request(
        &self,
        conn_id: ConnectionId,
        request: http::Request<NqBody>,
    ) -> OneshotResult<Response<Incoming>> {
        self.as_ref().send_request(conn_id, request)
    }
}
