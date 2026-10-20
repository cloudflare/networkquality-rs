// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use crate::nq_core::{
    ConnectionType, EstablishedConnection, OneshotResult, Time, Timestamp, body::NqBody,
};
use anyhow::Context;
use http::Response;
use hyper::body::Incoming;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::warn;

/// How long a connection to one resolved address may take before
/// [`connect_first_reachable`] moves on to the next address.
pub const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

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

/// Connects to the first of `addrs` that accepts a connection, trying them in order.
///
/// Every address but the last gets [`CONNECT_ATTEMPT_TIMEOUT`]; the last one runs until
/// `network` gives up. The first attempt's timing starts at `start`. Each later attempt's
/// timing starts when that attempt does, so the connection timing doesn't include
/// addresses that failed.
///
/// # Errors
///
/// Returns an error if `addrs` is empty, or if the connection to the last address fails.
pub async fn connect_first_reachable(
    network: &dyn Network,
    time: &dyn Time,
    start: Timestamp,
    addrs: &[SocketAddr],
    domain: &str,
    conn_type: ConnectionType,
) -> anyhow::Result<Arc<RwLock<EstablishedConnection>>> {
    let (last, earlier) = addrs
        .split_last()
        .context("host resolved to no addresses")?;

    let mut attempt_start = start;
    for addr in earlier {
        let attempt = network.new_connection(attempt_start, *addr, domain.to_owned(), conn_type);
        match tokio::time::timeout(CONNECT_ATTEMPT_TIMEOUT, attempt).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(error)) => warn!(%addr, "connection failed, trying the next address: {error:#}"),
            Err(_) => warn!(
                %addr,
                "no connection after {CONNECT_ATTEMPT_TIMEOUT:?}, trying the next address"
            ),
        }
        attempt_start = time.now();
    }

    network
        .new_connection(attempt_start, *last, domain.to_owned(), conn_type)
        .await
}
