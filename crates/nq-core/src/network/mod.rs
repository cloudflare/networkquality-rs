// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use crate::{body::NqBody, ConnectionType, EstablishedConnection, OneshotResult, Timestamp};
use http::Response;
use hyper::body::Incoming;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A network abstraction for resolving hosts, creating connections, and sending requests.
pub trait Network: Send + Sync + 'static {
    /// Resolves a host to a list of socket addresses.
    fn resolve(&self, host: String) -> OneshotResult<Vec<SocketAddr>>;

    /// Creates a new connection to the given domain.
    fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> OneshotResult<Arc<RwLock<EstablishedConnection>>>;

    /// Sends a request over the specified connection.
    fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
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
    ) -> OneshotResult<Arc<RwLock<EstablishedConnection>>> {
        self.as_ref()
            .new_connection(start, remote_addr, domain, conn_type)
    }

    fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
        request: http::Request<NqBody>,
    ) -> OneshotResult<Response<Incoming>> {
        self.as_ref().send_request(connection, request)
    }
}
