use std::fmt::Debug;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use boring::ssl::{SslConnector, SslMethod, SslVerifyMode};
use http::{Request, Response};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::TokioIo;
use tracing::{debug, error, info, Instrument};

use crate::body::NqBody;
use crate::network::ConnectionTiming;
use crate::util::ByteStream;
use crate::{ConnectionType, ResponseFuture, Time};

pub type TlsStream = tokio_boring::SslStream<Box<dyn ByteStream>>;

/// An [`EstablishedConnection`] contains the connection's timing and a handle
/// to send HTTP requests with.
#[derive(Debug)]
pub struct EstablishedConnection {
    timing: ConnectionTiming,
    send_request: SendRequest,
}

impl EstablishedConnection {
    pub fn new(timing: ConnectionTiming, send_request: SendRequest) -> Self {
        Self {
            timing,
            send_request,
        }
    }

    pub fn send_request(&mut self, req: Request<NqBody>) -> ResponseFuture {
        self.send_request.send_request(req)
    }

    pub fn timing(&self) -> ConnectionTiming {
        self.timing
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
        .map_err(|_| anyhow::anyhow!("unable to create tls stream"))?;

    timing.set_secure(time.now());

    debug!("created tls connection");

    Ok(ssl_stream)
}

#[tracing::instrument(skip(io, time))]
pub async fn start_h1_conn(
    domain: String,
    mut timing: ConnectionTiming,
    io: impl ByteStream,
    time: impl Time,
) -> anyhow::Result<EstablishedConnection> {
    let (send_request, connection) = http1::handshake(TokioIo::new(io)).await?;
    timing.set_application(time.now());
    // let time_handshake = started_at.elapsed();

    tokio::spawn({
        let domain = domain.clone();
        async move {
            if let Err(e) = connection.await {
                eprintln!("[{domain}] error: {e}");
            }
        }
    });

    let established_connection = EstablishedConnection::new(
        timing,
        SendRequest::H1 {
            dispatch: send_request,
        },
    );

    Ok(established_connection)
}

#[tracing::instrument(skip(io, time))]
pub async fn start_h2_conn(
    addr: SocketAddr,
    domain: String,
    mut timing: ConnectionTiming,
    io: impl ByteStream,
    time: &dyn Time,
) -> anyhow::Result<EstablishedConnection> {
    let (dispatch, connection) = http2::handshake(TokioExecutor, TokioIo::new(io)).await?;
    timing.set_application(time.now());

    debug!("finished h2 handshake");

    tokio::spawn(
        async move {
            if let Err(e) = connection.await {
                error!(error=%e, "error running connection");
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
