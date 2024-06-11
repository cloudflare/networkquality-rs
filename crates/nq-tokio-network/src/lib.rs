use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use async_trait::async_trait;
use anyhow::Result;

use http::{Response};
use hyper::body::Incoming;
use nq_core::{
    ConnectionManager, ConnectionTiming, ConnectionType, Network,
    NqBody, ResponseFuture, Time, Timestamp, EstablishedConnection
};

use tokio::net::TcpStream;
use tracing::{info};


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

#[async_trait]
impl Network for TokioNetwork {
    async fn resolve(&self, host: String) -> Result<Vec<SocketAddr>> {
        let time = self.inner.time.clone();
        timed_lookup_host(host, time).await.map_err(|e| e.into())
    }

    async fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> Result<Arc<RwLock<EstablishedConnection>>> {
        self.inner.new_connection(start, remote_addr, domain, conn_type).await
    }

    #[tracing::instrument(skip(self, request), fields(uri=%request.uri()))]
    async fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
        request: http::Request<NqBody>,
    ) -> Result<Response<Incoming>> {
        let inner = self.inner.clone();

        info!("sending request");

        let response_result = match inner.send_request(connection, request).await {
            Ok(fut) => fut.await,
            Err(error) => return Err(error.into()),
        };

        match response_result {
            Ok(response) => Ok(response),
            Err(error) => Err(error.into()),
        }
    }
}


#[derive(Clone)]
pub struct TokioNetworkInner {
    connections: Arc<ConnectionManager>,
    time: Arc<dyn Time>,
}

impl TokioNetworkInner {
    pub fn new(time: Arc<dyn Time>) -> Self {
        let connections: Arc<ConnectionManager> = Default::default();

        Self {
            connections,
            time,
        }
    }

    async fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> Result<Arc<RwLock<EstablishedConnection>>> {
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
            )
            .await?;

        Ok(connection)
    }

    #[tracing::instrument(skip(self, request), fields(uri=%request.uri()))]
    async fn send_request(
        &self,
        connection: Arc<RwLock<EstablishedConnection>>,
        request: http::Request<NqBody>,
    ) -> Result<ResponseFuture> {
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
