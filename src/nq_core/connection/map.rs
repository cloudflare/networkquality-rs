// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::nq_core::connection::http::{
    EstablishedConnection, start_h1_conn, start_h2_conn, tls_connection,
};
use crate::nq_core::util::ByteStream;
use crate::nq_core::{ConnectionTiming, ConnectionType, Time};

/// Creates and holds [`EstablishedConnection`]s in a VecDeque.
#[derive(Default, Debug)]
pub struct ConnectionManager {
    connections: RwLock<VecDeque<Arc<RwLock<EstablishedConnection>>>>,
}

impl ConnectionManager {
    /// Creates a new connection on the given io.
    #[allow(clippy::too_many_arguments)]
    pub async fn new_connection(
        &self,
        mut timing: ConnectionTiming,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
        io: Box<dyn ByteStream>,
        time: &dyn Time,
        shutdown: CancellationToken,
    ) -> Result<Arc<RwLock<EstablishedConnection>>> {
        let connection = match conn_type {
            ConnectionType::H1 { use_tls } => {
                if use_tls {
                    let stream = tls_connection(conn_type, &domain, &mut timing, io, time).await?;
                    start_h1_conn(domain, timing, stream, time, shutdown).await?
                } else {
                    start_h1_conn(domain, timing, io, time, shutdown).await?
                }
            }
            ConnectionType::H2 => {
                let stream = tls_connection(conn_type, &domain, &mut timing, io, time).await?;
                start_h2_conn(remote_addr, domain, timing, stream, time, shutdown).await?
            }
            ConnectionType::H3 => todo!(),
        };

        let connection = Arc::new(RwLock::new(connection));
        self.connections.write().await.push_back(connection.clone());
        Ok(connection)
    }

    /// Drop all `SendRequest` structs, effectively cancelling all connections.
    pub async fn shutdown(&self) {
        for connection in self.connections.write().await.iter_mut() {
            let mut conn = connection.write().await;
            conn.drop_send_request();
        }
    }
}
