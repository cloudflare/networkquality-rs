use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;

use http::Request;
use http_body_util::combinators::BoxBody;
use hyper::body::Bytes;
use tokio::sync::RwLock;

use crate::connection::http::{start_h2_conn, tls_connection};
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
    ) -> anyhow::Result<NewConnection> {
        match conn_type {
            crate::ConnectionType::H1 => todo!(),
            crate::ConnectionType::H2 => {
                let stream = tls_connection(conn_type, &domain, &mut timing, io, time).await?;

                let connection = start_h2_conn(remote_addr, domain, timing, stream, time).await?;
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

        connections
            .get_mut(&conn_id)
            .map(|conn| conn.send_request(request))
    }

    /// The number of [`EstablishedConnection`]s being held in the map.
    pub async fn len(&self) -> usize {
        self.map.read().await.len()
    }

    /// Returns if the [`ConnectionMap`] is empty.
    pub async fn is_empty(&self) -> bool {
        self.map.read().await.is_empty()
    }
}
