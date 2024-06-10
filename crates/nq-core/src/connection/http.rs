use std::fmt::Debug;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use boring::ssl::{SslConnector, SslMethod, SslVerifyMode};
use http::{Request, Response};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::TokioIo;
use shellflip::ShutdownSignal;
use tokio::select;
use tracing::{debug, error, info, Instrument};

use crate::body::NqBody;
use crate::util::ByteStream;
use crate::{ConnectionTiming, ConnectionType, ResponseFuture, Time};

pub type TlsStream = tokio_boring::SslStream<Box<dyn ByteStream>>;

/// An [`EstablishedConnection`] contains the connection's timing and a handle
/// to send HTTP requests with.
#[derive(Debug)]
pub struct EstablishedConnection {
    timing: ConnectionTiming,
    send_request: Option<SendRequest>,
}

/// Represents an established connection with timing information and a send request handler.
impl EstablishedConnection {
    /// Creates a new `EstablishedConnection`.
    pub fn new(timing: ConnectionTiming, send_request: SendRequest) -> Self {
        Self {
            timing,
            send_request: Some(send_request),
        }
    }

    /// Sends a request using the connection.
    pub fn send_request(&mut self, req: Request<NqBody>) -> Option<ResponseFuture> {
        self.send_request.as_mut().map(|s| s.send_request(req))
    }

    /// Returns the timing information of the connection.
    pub fn timing(&self) -> ConnectionTiming {
        self.timing
    }

    /// Drops the send request handler.
    pub fn drop_send_request(&mut self) {
        self.send_request = None;
    }
}

#[tracing::instrument(skip(io, time))]
pub async fn tls_connection(
    conn_type: ConnectionType,
    domain: &str,
    timing: &mut ConnectionTiming,
    io: impl ByteStream,
    time: &dyn Time,
) -> anyhow::Result<TlsStream> {
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;

    builder.cert_store_mut().set_default_paths()?;
    builder.set_verify(SslVerifyMode::PEER);

    let alpn: &[u8] = match conn_type {
        ConnectionType::H1 => b"\x08http/1.1",
        ConnectionType::H2 => b"\x02h2",
        ConnectionType::H3 => b"\x02h3",
    };

    builder.set_alpn_protos(alpn)?;
    let config = builder.build().configure()?;

    let ssl_stream = tokio_boring::connect(config, domain, Box::new(io) as Box<dyn ByteStream>)
        .await
        .map_err(|e| anyhow::anyhow!("unable to create tls stream: {e}"))?;

    timing.set_secure(time.now());

    debug!("created tls connection");

    Ok(ssl_stream)
}

#[tracing::instrument(skip(io, time, shutdown_signal))]
pub async fn start_h1_conn(
    domain: String,
    mut timing: ConnectionTiming,
    io: impl ByteStream,
    time: &dyn Time,
    mut shutdown_signal: ShutdownSignal,
) -> anyhow::Result<EstablishedConnection> {
    let (send_request, connection) = http1::handshake(TokioIo::new(io)).await?;
    timing.set_application(time.now());

    tokio::spawn(
        async move {
            select! {
                Err(e) = connection => {
                    error!(error=%e, "error running h1 connection");
                }
                _ = shutdown_signal.on_shutdown() => {
                    debug!("shutting down h1 connection");
                }
            }

            info!("connection finished");
        }
        .in_current_span(),
    );

    let established_connection = EstablishedConnection::new(
        timing,
        SendRequest::H1 {
            dispatch: send_request,
        },
    );

    Ok(established_connection)
}

#[tracing::instrument(skip(timing, io, time, shutdown_signal))]
pub async fn start_h2_conn(
    addr: SocketAddr,
    domain: String,
    mut timing: ConnectionTiming,
    io: impl ByteStream,
    time: &dyn Time,
    mut shutdown_signal: ShutdownSignal,
) -> anyhow::Result<EstablishedConnection> {
    let (dispatch, connection) = http2::handshake(TokioExecutor, TokioIo::new(io)).await?;
    timing.set_application(time.now());

    debug!("finished h2 handshake");

    tokio::spawn(
        async move {
            select! {
                Err(e) = connection => {
                    error!(error=%e, "error running h2 connection");
                }
                _ = shutdown_signal.on_shutdown() => {
                    debug!("shutting down h2 connection");
                }
            }

            info!("connection finished");
        }
        .in_current_span(),
    );

    info!(?timing, "established connection");
    let established_connection = EstablishedConnection::new(timing, SendRequest::H2 { dispatch });

    Ok(established_connection)
}

#[derive(Debug)]
pub enum SendRequest {
    #[allow(unused)]
    H1 {
        dispatch: http1::SendRequest<NqBody>,
    },
    H2 {
        dispatch: http2::SendRequest<NqBody>,
    },
}

impl SendRequest {
    fn send_request(
        &mut self,
        req: Request<NqBody>,
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
