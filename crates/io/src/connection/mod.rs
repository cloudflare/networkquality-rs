use std::{convert::Infallible, future::Future, net::SocketAddr, pin::Pin, time::Duration};

use http::{Request, Response};
use http_body_util::combinators::BoxBody;
use hyper::{
    body::{Bytes, Incoming},
    client::conn::{http1, http2},
};
use slotmap::SlotMap;
use tokio::{sync::RwLock, time::Instant};
use tracing::{debug, error, info, Instrument};

use crate::{
    tls::{tls_connection, TlsTiming},
    util::{ByteStream, ResponseFuture},
    ConnectionEstablished, ConnectionId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionType {
    H1,
    H2,
    H3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionTiming {
    pub started_at: Instant,
    pub time_connect: Duration,
    pub time_secure: Duration,
    pub time_handshake: Duration,
    pub time_total: Duration,
}

impl ConnectionTiming {
    pub fn new(time_connect: Duration, tls_timing: TlsTiming, time_handshake: Duration) -> Self {
        Self {
            started_at: tls_timing.started_at,
            time_connect,
            time_secure: tls_timing.time_secure,
            time_handshake,
            time_total: Duration::ZERO,
        }
    }
}

pub struct EstablishedConnection {
    timing: ConnectionTiming,
    send_request: SendRequest,
    // fd: RawFd,
}

impl EstablishedConnection {
    pub fn new(timing: ConnectionTiming, send_request: SendRequest) -> Self {
        Self {
            timing,
            send_request,
            // fd,
        }
    }

    pub fn send_request(&mut self, req: Request<BoxBody<Bytes, Infallible>>) -> ResponseFuture {
        self.send_request.send_request(req)
    }

    pub fn timing(&self) -> ConnectionTiming {
        self.timing
    }
}

pub enum SendRequest {
    H1 {
        dispatch: http1::SendRequest<BoxBody<Bytes, Infallible>>,
    },
    H2 {
        dispatch: http2::SendRequest<BoxBody<Bytes, Infallible>>,
    },
}

impl SendRequest {
    fn send_request(
        &mut self,
        req: Request<BoxBody<Bytes, Infallible>>,
    ) -> Pin<Box<dyn Future<Output = hyper::Result<Response<Incoming>>> + Send>> {
        match self {
            SendRequest::H1 {
                dispatch: send_request,
            } => Box::pin(send_request.send_request(req)),
            SendRequest::H2 {
                dispatch: send_request,
            } => Box::pin(send_request.send_request(req)),
        }
    }
}

pub async fn start_h1_conn(
    started_at: Instant,
    _addr: SocketAddr,
    domain: String,
    stream: Box<dyn ByteStream>,
    time_connect: Duration,
    tls_timing: TlsTiming,
) -> anyhow::Result<EstablishedConnection> {
    let (send_request, connection) = http1::handshake(stream).await?;
    let time_handshake = started_at.elapsed();

    let domain_clone = domain.to_string();
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("[{domain_clone}] error: {e}");
        }
    });

    let timing = ConnectionTiming::new(time_connect, tls_timing, time_handshake);
    let established_connection = EstablishedConnection::new(
        timing,
        SendRequest::H1 {
            dispatch: send_request,
        },
    );

    Ok(established_connection)
}

#[tracing::instrument(skip(started_at, stream, tls_timing))]
pub async fn start_h2_conn(
    started_at: Instant,
    addr: SocketAddr,
    domain: String,
    stream: Box<dyn ByteStream>,
    tls_timing: TlsTiming,
    time_connect: Duration,
) -> anyhow::Result<EstablishedConnection> {
    let (dispatch, connection) = http2::handshake(TokioExecutor, stream).await?;
    let time_handshake = started_at.elapsed();

    debug!(?time_handshake, "finished h2 handshake");

    tokio::spawn(
        async move {
            if let Err(e) = connection.await {
                error!(error=%e, "error running connection");
            }

            info!("connection finished");
        }
        .in_current_span(),
    );

    let timing = ConnectionTiming::new(time_connect, tls_timing, time_handshake);
    let established_connection = EstablishedConnection {
        timing,
        send_request: SendRequest::H2 { dispatch },
    };

    info!(?timing, "established connection");

    Ok(established_connection)
}

#[derive(Default)]
pub struct Connections {
    connections: RwLock<SlotMap<ConnectionId, EstablishedConnection>>,
}

impl Connections {
    pub async fn new_connection(
        &self,
        args: crate::NewConnectionArgs,
        io: Box<dyn ByteStream>,
        time_connect: Duration,
    ) -> anyhow::Result<ConnectionEstablished> {
        match args.conn_type {
            crate::ConnectionType::H1 => todo!(),
            crate::ConnectionType::H2 => {
                let (tls_timing, stream) = tls_connection(
                    args.start,
                    args.remote_addr,
                    &args.domain,
                    args.conn_type,
                    io,
                )
                .await?;

                let connection = start_h2_conn(
                    args.start,
                    args.remote_addr,
                    args.domain,
                    Box::new(stream),
                    tls_timing,
                    time_connect,
                )
                .await?;
                let timing = connection.timing();
                let id = self.connections.write().await.insert(connection);

                Ok(ConnectionEstablished { id, timing })
            }
            crate::ConnectionType::H3 => todo!(),
        }
    }

    pub async fn send_request(
        &self,
        conn_id: ConnectionId,
        request: http::Request<BoxBody<Bytes, Infallible>>,
    ) -> Option<ResponseFuture> {
        let mut connections = self.connections.write().await;

        connections
            .get_mut(conn_id)
            .map(|conn| conn.send_request(request))
    }

    pub async fn len(&self) -> usize {
        self.connections.read().await.len()
    }

    pub async fn insert(&self, connection: EstablishedConnection) -> ConnectionId {
        self.connections.write().await.insert(connection)
    }
}

#[derive(Clone)]
struct TokioExecutor;

impl<F> hyper::rt::Executor<F> for TokioExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        tokio::spawn(future);
    }
}
