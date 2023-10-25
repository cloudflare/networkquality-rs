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
use tokio::net::TcpStream;
use tracing::{error, info, Instrument};

#[derive(Debug, Clone)]
pub struct TokioNetwork {
    inner: TokioNetworkInner,
}

impl TokioNetwork {
    pub fn new(time: Arc<dyn Time>) -> Self {
        Self {
            inner: TokioNetworkInner::new(time),
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
}

impl TokioNetworkInner {
    pub fn new(time: Arc<dyn Time>) -> Self {
        Self {
            connections: Default::default(),
            time,
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

        let tcp_stream = TcpStream::connect(remote_addr).await?;
        tcp_stream.set_nodelay(false).unwrap();

        timing.set_connect(self.time.now());

        let connection = self
            .connections
            .new_connection(
                timing,
                remote_addr,
                domain,
                conn_type,
                Box::new(tcp_stream),
                &*self.time,
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
