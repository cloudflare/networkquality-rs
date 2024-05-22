use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::bail;
use http::{Request, Response};
use hyper::body::Incoming;
use nq_core::{
    oneshot_result, ConnectionId, ConnectionMap, ConnectionTiming, ConnectionType, Network,
    NewConnection, NqBody, OneshotResult, ResponseFuture, Time, Timestamp,
};
use shellflip::{ShutdownHandle, ShutdownSignal};
use tokio::net::TcpStream;
use tracing::{error, info, Instrument};

#[derive(Debug, Clone)]
pub struct TokioNetwork {
    inner: TokioNetworkInner,
}

impl TokioNetwork {
    pub fn new(time: Arc<dyn Time>, shutdown: Arc<ShutdownHandle>) -> Self {
        Self {
            inner: TokioNetworkInner::new(time, shutdown),
        }
    }
}

impl Network for TokioNetwork {
    fn resolve(&self, host: String) -> OneshotResult<Vec<SocketAddr>> {
        let (tx, rx) = oneshot_result();

        tokio::spawn(async move {
            let result = tokio::net::lookup_host(host)
                .await
                .map(|iter| iter.collect())
                .map_err(Into::into);

            let _ = tx.send(result);
        });

        rx
    }

    fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> OneshotResult<NewConnection> {
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
        conn_id: ConnectionId,
        request: Request<NqBody>,
    ) -> OneshotResult<Response<Incoming>> {
        let (tx, rx) = oneshot_result();

        let inner = self.inner.clone();
        tokio::spawn(
            async move {
                info!("sending request");

                let response_result = match inner.send_request(conn_id, request).await {
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
    connections: Arc<ConnectionMap>,
    time: Arc<dyn Time>,
    shutdown: Arc<ShutdownHandle>,
}

impl TokioNetworkInner {
    pub fn new(time: Arc<dyn Time>, shutdown: Arc<ShutdownHandle>) -> Self {
        let connections: Arc<ConnectionMap> = Default::default();

        tokio::spawn({
            let connections = Arc::clone(&connections);
            let mut signal = ShutdownSignal::from(&*shutdown);

            async move {
                signal.on_shutdown().await;
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
    ) -> anyhow::Result<NewConnection> {
        let mut timing = ConnectionTiming::new(start);

        // Measure DNS resolution time
        let dns_start = self.time.now();
        let _ = tokio::net::lookup_host(&domain).await?;
        let dns_end = self.time.now();
        let dns_duration = dns_end.duration_since(dns_start);
        info!("DNS lookup for {} took {:?}", domain, dns_duration);

        // Update the connection timing with DNS duration
        timing.set_dns_lookup(dns_duration);

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
                ShutdownSignal::from(&*self.shutdown),
            )
            .await?;

        Ok(connection)
    }

    #[tracing::instrument(skip(self, request), fields(uri=%request.uri()))]
    async fn send_request(
        &self,
        conn_id: ConnectionId,
        request: http::Request<NqBody>,
    ) -> anyhow::Result<ResponseFuture> {
        info!("sending request");

        let Some(response_fut) = self.connections.send_request(conn_id, request).await else {
            bail!("ConnectionId={conn_id:?} does not exist");
        };

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
