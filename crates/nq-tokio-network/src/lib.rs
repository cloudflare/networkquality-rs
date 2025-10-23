// Copyright (c) 2017-2020 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

use http::{Request, Response};
use hyper::body::Incoming;
use nq_core::{
    ConnectionManager, ConnectionTiming, ConnectionType, EstablishedConnection, Network, NqBody,
    OneshotResult, ResponseFuture, Time, Timestamp, oneshot_result,
};

use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info};

#[derive(Debug, Clone)]
pub struct TokioNetwork {
    inner: TokioNetworkInner,
}

impl TokioNetwork {
    pub fn new(time: Arc<dyn Time>, shutdown: CancellationToken) -> Self {
        Self {
            inner: TokioNetworkInner::new(time, shutdown),
        }
    }
}

impl Network for TokioNetwork {
    fn resolve(&self, host: String) -> OneshotResult<Vec<SocketAddr>> {
        let (tx, rx) = oneshot_result();
        let time = self.inner.time.clone();

        tokio::spawn(async move {
            match timed_lookup_host(host, time).await {
                Ok(addrs) => {
                    if tx.send(Ok(addrs)).is_err() {
                        error!("Failed to send resolved addresses");
                    }
                }
                Err(e) => {
                    if tx.send(Err(e)).is_err() {
                        error!("Failed to send error");
                    }
                }
            }
        });

        rx
    }

    fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> OneshotResult<Arc<RwLock<EstablishedConnection>>> {
        let (tx, rx) = oneshot_result();

        let inner = self.inner.clone();
        tokio::spawn(async move {
            let new_connection = inner
                .new_connection(start, remote_addr, domain, conn_type)
                .await;

            if tx.send(new_connection).is_err() {
                error!("unable to create connection");
            }
        });

        rx
    }

    #[tracing::instrument(skip(self, request), fields(uri=%request.uri()))]
    fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
        request: Request<NqBody>,
    ) -> OneshotResult<Response<Incoming>> {
        let (tx, rx) = oneshot_result();

        let inner = self.inner.clone();
        tokio::spawn(
            async move {
                info!("sending request");

                let response_result = match inner.send_request(connection, request).await {
                    Ok(fut) => fut.await,
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        return;
                    }
                };

                let response = match response_result {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = tx.send(Err(error.into()));
                        return;
                    }
                };

                info!("sending response future");
                let _ = tx.send(Ok(response));
            }
            .in_current_span(),
        );

        rx
    }
}

#[derive(Clone)]
pub struct TokioNetworkInner {
    connections: Arc<ConnectionManager>,
    time: Arc<dyn Time>,
    shutdown: CancellationToken,
}

impl TokioNetworkInner {
    pub fn new(time: Arc<dyn Time>, shutdown: CancellationToken) -> Self {
        let connections: Arc<ConnectionManager> = Default::default();

        tokio::spawn({
            let connections = Arc::clone(&connections);
            let cloned_shutdown = shutdown.clone();

            async move {
                cloned_shutdown.cancelled().await;
                info!("shutting down connections");
                connections.shutdown().await;
            }
        });

        Self {
            connections,
            time,
            shutdown,
        }
    }

    async fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> anyhow::Result<Arc<RwLock<EstablishedConnection>>> {
        let mut timing = ConnectionTiming::new(start);

        let tcp_stream = TcpStream::connect(remote_addr).await?;
        timing.set_connect(self.time.now());

        tcp_stream.set_nodelay(false).unwrap();

        let connection = self
            .connections
            .new_connection(
                timing,
                remote_addr,
                domain,
                conn_type,
                Box::new(tcp_stream),
                &*self.time,
                self.shutdown.clone(),
            )
            .await?;

        Ok(connection)
    }

    #[tracing::instrument(skip(self, request), fields(uri=%request.uri()))]
    async fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
        request: http::Request<NqBody>,
    ) -> anyhow::Result<ResponseFuture> {
        info!("sending request");

        let mut conn = connection.write().await;
        let response_fut = conn
            .send_request(request)
            .ok_or_else(|| anyhow::anyhow!("Failed to send request"))?;

        Ok(response_fut)
    }
}

impl Debug for TokioNetworkInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioNetworkInner")
            .field("connections", &self.connections)
            .field("time", &"Arc<dyn Time>")
            .finish()
    }
}

async fn timed_lookup_host(host: String, time: Arc<dyn Time>) -> anyhow::Result<Vec<SocketAddr>> {
    let dns_start = time.now();
    let addrs = tokio::net::lookup_host(host).await?.collect();
    let dns_end = time.now();
    let dns_duration = dns_end.duration_since(dns_start);

    let mut timing = ConnectionTiming::new(dns_start);
    timing.set_dns_lookup(dns_duration);

    Ok(addrs)
}
