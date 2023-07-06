use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
};

use http::{HeaderName, HeaderValue, Request, Uri};
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use tokio::{sync::oneshot, time::Instant};
use tracing::{error, info, Instrument};

use crate::{
    connection::Connections,
    util::{ConnectUpgraded, ResponseFuture},
    ConnectionType, Network, NewConnectionArgs,
};

pub struct ProxyConfig {
    pub endpoint: Uri,
    pub headers: HashMap<String, String>,
    pub conn_type: ConnectionType,
}

impl ProxyConfig {
    pub fn new(endpoint: Uri, conn_type: ConnectionType, headers: HashMap<String, String>) -> Self {
        Self {
            endpoint,
            headers,
            conn_type,
        }
    }

    pub fn connect(&self, remote_addr: SocketAddr, host: String) -> anyhow::Result<Request<()>> {
        let mut request = Request::connect(remote_addr.to_string());

        for (key, value) in self.headers.iter() {
            request = request.header(
                HeaderName::from_bytes(key.as_bytes())?,
                HeaderValue::from_bytes(value.as_bytes())?,
            );
        }

        request
            .header("Host", HeaderValue::from_bytes(host.as_bytes())?)
            .body(())
            .map_err(Into::into)
    }
}

pub struct ProxyNetwork<N> {
    inner: Arc<ProxyNetworkInner<N>>,
}

impl<N> ProxyNetwork<N> {
    pub fn new(network: N, config: ProxyConfig) -> Self {
        Self {
            inner: Arc::new(ProxyNetworkInner::new(network, config)),
        }
    }
}

pub struct ProxyNetworkInner<N> {
    network: N,
    proxy_config: ProxyConfig,
    connections: Connections,
}

impl<N> ProxyNetworkInner<N> {
    pub fn new(network: N, proxy_config: ProxyConfig) -> Self {
        Self {
            network,
            proxy_config,
            connections: Connections::default(),
        }
    }
}

impl<N: Network + Send + Sync + 'static> Network for ProxyNetwork<N> {
    #[tracing::instrument(skip(self, args), fields(remote_addr=?args.remote_addr, domain=args.domain, conn=?args.conn_type))]
    fn new_connection(
        &self,
        args: crate::NewConnectionArgs,
    ) -> crate::OneshotResult<crate::ConnectionEstablished> {
        let (tx, rx) = oneshot::channel();

        let proxy = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let start = Instant::now();

            let config = &proxy.proxy_config;
            let host = config.endpoint.host().unwrap();
            let port = config.endpoint.port().map(|p| p.as_u16()).unwrap_or(443);
            let domain = config.endpoint.host().unwrap().to_string();

            let proxy_addr = (host, port).to_socket_addrs()?.next().unwrap();

            let proxy_connection_args = NewConnectionArgs {
                start,
                remote_addr: proxy_addr,
                domain,
                conn_type: config.conn_type,
            };

            let proxy_connection = proxy
                .network
                .new_connection(proxy_connection_args)
                .await??;

            info!(conn_id=?proxy_connection.id, "created proxy connection");

            let connect_request = config
                .connect(args.remote_addr, args.domain.clone())?
                .map(|_| Empty::<Bytes>::new().boxed());

            info!("upgrading connection");
            let connect_start = Instant::now();
            let connect_response = proxy
                .network
                .send_request(connect_start, proxy_connection.id, connect_request)
                .await?? // definitely a better way here
                .await?;

            info!(status=?connect_response.status(), "connection request status");

            let upgraded = hyper::upgrade::on(connect_response).await?;

            info!("connection upgraded");

            let established_connection = proxy
                .connections
                .new_connection(
                    args,
                    Box::new(ConnectUpgraded::new(upgraded)),
                    connect_start.elapsed(),
                )
                .await?;

            info!("connection established");

            if tx.send(Ok(established_connection)).is_err() {
                // TODO(mark error)
            }

            Ok::<_, anyhow::Error>(())
        });

        rx
    }

    #[tracing::instrument(skip(self, request))]
    fn send_request(
        &self,
        started_at: tokio::time::Instant, // TODO(started_at)
        conn_id: crate::ConnectionId,
        request: http::Request<
            http_body_util::combinators::BoxBody<hyper::body::Bytes, std::convert::Infallible>,
        >,
    ) -> crate::OneshotResult<ResponseFuture> {
        let (req_resp_tx, req_resp_rx) = oneshot::channel();

        let inner = self.inner.clone();
        tokio::spawn(async move {
            info!("sending request");
            let Some(response_fut) = inner.connections.send_request(conn_id, request).await else {
                error!(?conn_id, "connection does not exist");
                let _ = req_resp_tx.send(Err(anyhow::anyhow!("{conn_id:?} does not exist")));
                return;
            };

            info!("sending response future");
            let _ = req_resp_tx.send(Ok(response_fut));
        }.in_current_span());

        req_resp_rx
    }
}
