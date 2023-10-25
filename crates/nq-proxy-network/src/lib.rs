use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::{anyhow, bail};
use http::{HeaderName, HeaderValue, Request, Response, Uri};
use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper_util::rt::TokioIo;
use nq_core::{
    oneshot_result, ConnectUpgraded, ConnectionId, ConnectionMap, ConnectionTiming, ConnectionType,
    Network, NewConnection, NqBody, OneshotResult, ResponseFuture, Time, Timestamp,
};
use tracing::{info, Instrument};

pub struct ProxyConfig {
    pub endpoint: Uri,
    pub headers: HashMap<String, String>,
    pub conn_type: ConnectionType,
    pub raw_public_key: Option<Vec<u8>>,
}

impl ProxyConfig {
    pub fn new(
        endpoint: Uri,
        conn_type: ConnectionType,
        headers: HashMap<String, String>,
        raw_public_key: Option<Vec<u8>>,
    ) -> Self {
        Self {
            endpoint,
            headers,
            conn_type,
            raw_public_key,
        }
    }

    pub fn connect(&self, target_host: &str, target_addr: &str) -> anyhow::Result<Request<()>> {
        let mut request = Request::connect(target_addr);

        for (key, value) in self.headers.iter() {
            request = request.header(
                HeaderName::from_bytes(key.as_bytes())?,
                HeaderValue::from_bytes(value.as_bytes())?,
            );
        }

        request
            .header("Host", HeaderValue::from_bytes(target_host.as_bytes())?)
            .body(())
            .map_err(Into::into)
    }
}

pub struct ProxyNetwork<N> {
    inner: Arc<ProxyNetworkInner<N>>,
}

impl<N: Network> ProxyNetwork<N> {
    pub fn new(config: ProxyConfig, network: N, time: Arc<dyn Time>) -> Self {
        Self {
            inner: Arc::new(ProxyNetworkInner::new(config, network, time)),
        }
    }
}

pub struct ProxyNetworkInner<N> {
    network: N,
    time: Arc<dyn Time>,
    config: ProxyConfig,
    connections: ConnectionMap,
}

impl<N: Network> ProxyNetworkInner<N> {
    pub fn new(config: ProxyConfig, network: N, time: Arc<dyn Time>) -> Self {
        Self {
            network,
            time,
            config,
            connections: ConnectionMap::default(),
        }
    }

    async fn new_proxy_connection(&self, target_host: &str) -> anyhow::Result<ConnectUpgraded> {
        let start = self.time.now();
        let proxy_authority = self
            .config
            .endpoint
            .authority()
            .ok_or_else(|| anyhow!("proxy endpoint must have a valid authority"))?;

        let proxy_host = self
            .config
            .endpoint
            .host()
            .ok_or_else(|| anyhow!("proxy endpoint most have a host"))?
            .to_string();

        // todo(fisher): handle username/passwd authorities
        // todo(fisher): handle resolve timing
        let proxy_addr = self
            .network
            .resolve(proxy_authority.as_str().to_string())
            .await?[0];

        let proxy_connection = self
            .network
            .new_connection(start, proxy_addr, proxy_host, self.config.conn_type)
            .await?;

        info!(conn_id=?proxy_connection.id, "created proxy connection");

        let connect_request = self
            .config
            // todo(fisher): allow the client network to resolve the target host, rather
            // than the proxy.
            .connect(target_host, target_host)?
            .map(|_| Empty::<Bytes>::new().boxed());

        info!("upgrading connection");

        // todo(fisher): Inject CONNECT timing somehow.
        let connect_response = self
            .network
            .send_request(proxy_connection.id, connect_request)
            .await?;

        info!(status=?connect_response.status(), "CONNECT request status");

        let upgraded = hyper::upgrade::on(connect_response).await?;

        Ok(ConnectUpgraded::new(TokioIo::new(upgraded)))
    }

    #[tracing::instrument(skip(self), fields(start, ?remote_addr, domain, ?conn_type))]
    async fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> anyhow::Result<NewConnection> {
        let tunnled_stream = self.new_proxy_connection(&domain).await?;

        // todo(fisher): include first hop timing in proxied network results (?)
        let timing = ConnectionTiming::new(start);
        let established_connection = self
            .connections
            .new_connection(
                timing,
                remote_addr,
                domain,
                conn_type,
                Box::new(tunnled_stream),
                &*self.time,
            )
            .await?;

        info!("connection established");
        Ok(established_connection)
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

impl<N: Network + Send + Sync + 'static> Network for ProxyNetwork<N> {
    fn resolve(&self, host: String) -> nq_core::OneshotResult<Vec<SocketAddr>> {
        self.inner.network.resolve(host)
    }

    #[tracing::instrument(skip(self), fields(start, ?remote_addr, domain, ?conn_type))]
    fn new_connection(
        &self,
        start: Timestamp,
        remote_addr: SocketAddr,
        domain: String,
        conn_type: ConnectionType,
    ) -> OneshotResult<NewConnection> {
        let (tx, rx) = oneshot_result();

        let proxy = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let res = proxy
                .new_connection(start, remote_addr, domain, conn_type)
                .await;

            let _ = tx.send(res);
        });

        rx
    }

    #[tracing::instrument(skip(self, request))]
    fn send_request(
        &self,
        conn_id: ConnectionId,
        request: http::Request<NqBody>,
    ) -> OneshotResult<Response<Incoming>> {
        // let (req_resp_tx, req_resp_rx) = oneshot::channel();
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
