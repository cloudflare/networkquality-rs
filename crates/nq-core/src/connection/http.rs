// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::fmt::Debug;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::bail;
use boring::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use boring::x509::X509;
use boring::x509::store::X509StoreBuilder;
use http::header::HOST;
use http::{HeaderValue, Request, Response};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::TokioIo;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, error, info, warn};

use crate::body::NqBody;
use crate::util::ByteStream;
use crate::{ConnectionTiming, ConnectionType, ResponseFuture, Time};

pub type TlsStream = tokio_boring::SslStream<Box<dyn ByteStream>>;

/// Process-wide switch to skip TLS certificate verification. Off by default.
///
/// This exists only to allow testing against servers presenting self-signed
/// certificates (e.g. a local `wrangler dev` speed-test server). It must never
/// be enabled against production endpoints.
static INSECURE_TLS: AtomicBool = AtomicBool::new(false);

/// Enable or disable skipping TLS certificate verification globally.
pub fn set_insecure_tls(insecure: bool) {
    INSECURE_TLS.store(insecure, Ordering::Relaxed);
}

/// Whether TLS certificate verification is currently being skipped.
pub fn insecure_tls() -> bool {
    INSECURE_TLS.load(Ordering::Relaxed)
}

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
    let mut builder = SslConnector::builder(SslMethod::tls())?;

    // Use platform CA certs
    let mut store_builder = X509StoreBuilder::new()?;
    if let Ok(ca_certs) = rustls_native_certs::load_native_certs() {
        for root in ca_certs {
            let _ = store_builder.add_cert(X509::from_der(&root)?);
        }
    }
    builder.set_verify_cert_store(store_builder.build())?;
    if insecure_tls() {
        debug!("TLS certificate verification disabled (insecure mode)");
        builder.set_verify(SslVerifyMode::NONE);
    } else {
        builder.set_verify(SslVerifyMode::PEER);
    }

    let alpn: &[u8] = match conn_type {
        ConnectionType::H1 { use_tls: false } => {
            bail!("cannot create tls connection if `use_tls: false`")
        }
        ConnectionType::H1 { use_tls: true } => b"\x08http/1.1",
        ConnectionType::H2 => b"\x02h2",
        ConnectionType::H3 => b"\x02h3",
    };

    builder.set_alpn_protos(alpn)?;
    let config = builder.build().configure()?;

    let ssl_stream = tokio_boring::connect(config, domain, Box::new(io) as Box<dyn ByteStream>)
        .await
        .map_err(|e| anyhow::anyhow!("unable to create tls stream: {e}"))?;

    timing.set_secure(time.now());

    // Normalize the TLS handshake time to the number of round-trips the
    // negotiated version takes (draft-ietf-ippm-responsiveness-09 §5.3).
    // TLS 1.3 completes in 1 round-trip, TLS 1.2 in 2. Default to 1.
    let tls_round_trips = match ssl_stream.ssl().version2() {
        Some(SslVersion::TLS1_3) => 1,
        Some(SslVersion::TLS1_2) => 2,
        _ => 1,
    };
    timing.set_tls_round_trips(tls_round_trips);

    debug!(tls_round_trips, "created tls connection");

    Ok(ssl_stream)
}

#[tracing::instrument(skip(io, time, shutdown))]
pub async fn start_h1_conn(
    domain: String,
    mut timing: ConnectionTiming,
    io: impl ByteStream,
    time: &dyn Time,
    shutdown: CancellationToken,
) -> anyhow::Result<EstablishedConnection> {
    let (send_request, connection) = http1::handshake(TokioIo::new(io)).await?;
    timing.set_application(time.now());

    tokio::spawn(
        async move {
            select! {
                Err(e) = connection => {
                    debug!(error=%e, "error running h1 connection");
                }
                _ = shutdown.cancelled() => {
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

#[tracing::instrument(skip(timing, io, time, shutdown))]
pub async fn start_h2_conn(
    addr: SocketAddr,
    domain: String,
    mut timing: ConnectionTiming,
    io: impl ByteStream,
    time: &dyn Time,
    shutdown: CancellationToken,
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
                _ = shutdown.cancelled() => {
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
        mut req: Request<NqBody>,
    ) -> Pin<Box<dyn Future<Output = hyper::Result<Response<Incoming>>> + Send>> {
        match self {
            SendRequest::H1 {
                dispatch: send_request,
            } => {
                // HTTP/1.1 to an origin server requires origin-form request
                // targets (`GET /path`) plus a `Host` header carrying the
                // authority. Building the request from an absolute URI leaves
                // it in proxy/absolute-form (`GET http://host/path`), which
                // origin servers (e.g. workerd) reject. Normalize here.
                Self::normalize_h1_request(&mut req);
                Box::pin(send_request.send_request(req))
            }
            SendRequest::H2 {
                dispatch: send_request,
            } => {
                // HTTP/2 uses the :authority pseudo-header derived from the
                // absolute URI, so leave the request untouched.
                Box::pin(send_request.send_request(req))
            }
        }
    }

    /// Rewrite an HTTP/1.1 request into origin-form with a proper `Host` header.
    fn normalize_h1_request(req: &mut Request<NqBody>) {
        // Set `Host` from the full authority (host and, if present, port).
        if !req.headers().contains_key(HOST) {
            if let Some(authority) = req.uri().authority().cloned() {
                if let Ok(host) = HeaderValue::from_str(authority.as_str()) {
                    req.headers_mut().insert(HOST, host);
                } else {
                    // HTTP/1.1 requires a Host header; without one the origin
                    // answers 400. An authority is already restricted to
                    // characters a header value accepts, so this is not
                    // expected to be reachable -- but a silent drop is
                    // near-impossible to diagnose from the far end.
                    warn!(
                        %authority,
                        "could not build a Host header from the URI authority; \
                         sending the request without one"
                    );
                }
            }
        }

        // Collapse the request target to origin-form (path + query only).
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str().to_owned())
            .unwrap_or_else(|| "/".to_owned());

        match path_and_query.parse::<http::Uri>() {
            Ok(uri) => *req.uri_mut() = uri,
            // Leaves the request in absolute-form, which origin servers
            // reject. The string comes from an already-validated
            // PathAndQuery so this is not expected to be reachable, but the
            // failure would otherwise be invisible.
            Err(error) => warn!(
                path_and_query,
                %error,
                "failed to parse origin-form URI; sending absolute-form"
            ),
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
