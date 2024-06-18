use std::net::SocketAddr;
use std::sync::Arc;
use http::Response;
use hyper::body::Incoming;
use tokio::sync::RwLock;
use async_trait::async_trait;
use anyhow::Result;
use crate::{body::NqBody, Timestamp, ConnectionType, EstablishedConnection};

/// A network abstraction for resolving hosts, creating connections, and sending requests.
#[async_trait]
pub trait Network: Send + Sync + 'static {
    /// Resolves a host to a list of socket addresses.
    async fn resolve(&self, host: String) -> Result<Vec<SocketAddr>>;

    /// Creates a new connection to the given domain.
    async fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> Result<Arc<RwLock<EstablishedConnection>>>;

    /// Sends a request over the specified connection.
    async fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
        request: http::Request<NqBody>,
    ) -> Result<Response<Incoming>>;
}

#[async_trait]
impl Network for Arc<dyn Network> {
    async fn resolve(&self, host: String) -> Result<Vec<SocketAddr>> {
        self.as_ref().resolve(host).await
    }

    async fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> Result<Arc<RwLock<EstablishedConnection>>> {
        self.as_ref()
            .new_connection(start, remote_addr, domain, conn_type)
            .await
    }

    async fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
        request: http::Request<NqBody>,
    ) -> Result<Response<Incoming>> {
        self.as_ref().send_request(connection, request)
            .await
    }
}
