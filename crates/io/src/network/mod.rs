pub mod os;
pub mod proxied;

use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use http_body_util::combinators::BoxBody;
use hyper::body::Bytes;
use tokio::time::Instant;

use crate::{
    connection::ConnectionTiming,
    util::{OneshotResult, ResponseFuture},
    ConnectionType,
};

slotmap::new_key_type! {
    pub struct ConnectionId;
}

#[derive(Debug)]
pub struct NewConnectionArgs {
    pub start: Instant,
    pub remote_addr: SocketAddr,
    pub domain: String,
    pub conn_type: ConnectionType,
}

impl NewConnectionArgs {
    pub fn new(
        start: Instant,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> Self {
        Self {
            start,
            remote_addr,
            domain,
            conn_type,
        }
    }

    pub fn h1(start: Instant, remote_addr: SocketAddr, domain: String) -> Self {
        Self::new(start, remote_addr, domain, ConnectionType::H1)
    }

    pub fn h2(start: Instant, remote_addr: SocketAddr, domain: String) -> Self {
        Self::new(start, remote_addr, domain, ConnectionType::H2)
    }

    pub fn h3(start: Instant, remote_addr: SocketAddr, domain: String) -> Self {
        Self::new(start, remote_addr, domain, ConnectionType::H3)
    }
}

pub trait Network: Send + Sync + 'static {
    fn new_connection(&self, args: NewConnectionArgs) -> OneshotResult<ConnectionEstablished>;
    fn send_request(
        &self,
        started_at: Instant,
        conn_id: ConnectionId,
        request: http::Request<BoxBody<Bytes, Infallible>>,
    ) -> OneshotResult<ResponseFuture>;
}

pub struct ConnectionEstablished {
    pub id: ConnectionId,
    pub timing: ConnectionTiming,
}

impl<N: Network> Network for Arc<N> {
    fn new_connection(&self, args: NewConnectionArgs) -> OneshotResult<ConnectionEstablished> {
        (**self).new_connection(args)
    }

    fn send_request(
        &self,
        started_at: Instant,
        conn_id: ConnectionId,
        request: http::Request<BoxBody<Bytes, Infallible>>,
    ) -> OneshotResult<ResponseFuture> {
        (**self).send_request(started_at, conn_id, request)
    }
}
