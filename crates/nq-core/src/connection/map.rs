// todo(fisher): cleanup constructor arguments
#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;

use http::Request;
use http_body_util::combinators::BoxBody;
use hyper::body::Bytes;
use shellflip::ShutdownSignal;
use tokio::sync::RwLock;
use tracing::info;

use crate::connection::http::{start_h1_conn, start_h2_conn, tls_connection};
use crate::connection::NewConnection;
use crate::network::ConnectionTiming;
use crate::util::ByteStream;
use crate::{ConnectionId, ConnectionType, ResponseFuture, Time};

use super::http::EstablishedConnection;

/// Creates and holds [`EstablishedConnection`]s in a map.
#[derive(Default, Debug)]
pub struct ConnectionMap {
    map: RwLock<HashMap<ConnectionId, EstablishedConnection>>,
}

impl ConnectionMap {
    /// Creates a new connection on the given io.
    pub async fn new_connection(
        &self,
        mut timing: ConnectionTiming,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
        io: Box<dyn ByteStream>,
        time: &dyn Time,
        shutdown_signal: ShutdownSignal,
    ) -> anyhow::Result<NewConnection> {
        match conn_type {
            crate::ConnectionType::H1 => {
                let connection = start_h1_conn(domain, timing, io, time, shutdown_signal).await?;
                let timing = connection.timing();

                let id = ConnectionId::new();
                self.map.write().await.insert(id, connection);

                Ok(NewConnection { id, timing })
            }
            crate::ConnectionType::H2 => {
                let stream = tls_connection(conn_type, &domain, &mut timing, io, time).await?;

                let connection =
                    start_h2_conn(remote_addr, domain, timing, stream, time, shutdown_signal)
                        .await?;
                let timing = connection.timing();

                let id = ConnectionId::new();
                self.map.write().await.insert(id, connection);

                Ok(NewConnection { id, timing })
            }
            crate::ConnectionType::H3 => todo!(),
        }
    }

    /// Sends a request on the given connection.
    pub async fn send_request(
        &self,
        conn_id: ConnectionId,
        request: Request<BoxBody<Bytes, Infallible>>,
    ) -> Option<ResponseFuture> {
        let mut connections = self.map.write().await;

        info!("sending request on conn_id={conn_id:?}");

        connections
            .get_mut(&conn_id)
            .and_then(|conn| conn.send_request(request))
    }

    /// The number of [`EstablishedConnection`]s being held in the map.
    pub async fn len(&self) -> usize {
        self.map.read().await.len()
    }

    /// Returns if the [`ConnectionMap`] is empty.
    pub async fn is_empty(&self) -> bool {
        self.map.read().await.is_empty()
    }

    /// Drop all `SendRequest` structs, effectively cancelling all connections.
    pub async fn shutdown(&self) {
        for (_, connection) in self.map.write().await.iter_mut() {
            connection.drop_send_request();
        }
    }
}
