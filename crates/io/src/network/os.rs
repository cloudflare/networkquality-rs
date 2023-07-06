use std::{convert::Infallible, sync::Arc};

use anyhow::anyhow;
use http_body_util::combinators::BoxBody;
use hyper::body::Bytes;
use tokio::{net::TcpStream, sync::oneshot, time::Instant};
use tracing::{error, info, Instrument};

use crate::{connection::Connections, util::ResponseFuture, ConnectionId, OneshotResult};

use super::{ConnectionEstablished, Network};

#[derive(Default)]
pub struct OSNetwork {
    inner: TokioNetworkInner,
}

impl Network for OSNetwork {
    fn new_connection(
        &self,
        args: crate::NewConnectionArgs,
    ) -> OneshotResult<ConnectionEstablished> {
        let (tx, rx) = oneshot::channel();

        let inner = self.inner.clone();
        tokio::spawn(async move {
            let tcp_stream = match TcpStream::connect(args.remote_addr).await {
                Ok(stream) => stream,
                Err(e) => {
                    let _ = tx.send(Err(e.into()));
                    return;
                }
            };
            tcp_stream.set_nodelay(false).unwrap();

            let time_connect = args.start.elapsed();

            let connection = inner
                .connections
                .new_connection(args, Box::new(tcp_stream), time_connect)
                .await;

            if tx.send(connection).is_err() {
                error!("unable to create connection");
            }
        });

        rx
    }

    #[tracing::instrument(skip(self, request), fields(uri=%request.uri()))]
    fn send_request(
        &self,
        start: Instant,
        conn_id: ConnectionId,
        request: http::Request<BoxBody<Bytes, Infallible>>,
    ) -> crate::OneshotResult<ResponseFuture> {
        let (req_resp_tx, req_resp_rx) = oneshot::channel();

        let inner = self.inner.clone();
        tokio::spawn(async move {
            info!("sending request");
            let Some(response_fut) = inner.connections.send_request(conn_id, request).await else {
                error!(?conn_id, "connection does not exist");
                let _ = req_resp_tx.send(Err(anyhow!("{conn_id:?} does not exist")));
                return;
            };

            info!("sending response future");
            let _ = req_resp_tx.send(Ok(response_fut));
        }.in_current_span());

        req_resp_rx
    }
}

#[derive(Default, Clone)]
pub struct TokioNetworkInner {
    connections: Arc<Connections>,
}
