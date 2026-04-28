// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Defines two clients, a [`ThroughputClient`] and a normal [`Client`]. The
//! [`ThroughputClient`] trackes the sending or receiving of body data and sends
//! byte count updates to a listener. This is useful for determining the
//! throughput of a flow.

use std::{convert::Infallible, net::ToSocketAddrs, sync::Arc, time::Duration};
use tokio::sync::RwLock;

use anyhow::Context;
use http::{HeaderMap, HeaderValue, Uri};
use http_body_util::BodyExt;
use hyper::body::{Body, Bytes, Incoming};
use tokio::select;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, Instrument};

use crate::{
    body::{empty, BodyEvent, CountingBody, InflightBody, NqBody, UploadBody},
    oneshot_result, ConnectionType, EstablishedConnection, Network, OneshotResult, Time, Timestamp,
};

/// The default user agent for networkquality requests
pub const MACH_USER_AGENT: &str = "mach/0.1.0";

/// Describes the direction of the client. This determines if the client times
/// the upload or download of a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Download the response body.
    Down,
    /// Upload the given number of bytes.
    Up(usize),
}

/// A [`ThroughputClient`] is a simple client which drives a request/response pair
/// and returns an [`InflightBody`].
///
/// This should be used if you do not care about the request or response, and just
/// need to load a connection.
///
/// The returned [`InflightBody`] can be used to track the progress of an upload
/// or download and when it finishes.
pub struct ThroughputClient {
    connection: Option<Arc<RwLock<EstablishedConnection>>>,
    new_connection_type: Option<ConnectionType>,
    headers: Option<HeaderMap>,
    direction: Direction,
    plain_http_mode: bool,
}

impl ThroughputClient {
    /// Create an download oriented [`ThroughputClient`].
    pub fn download() -> Self {
        Self {
            connection: None,
            new_connection_type: None,
            headers: None,
            direction: Direction::Down,
            plain_http_mode: false,
        }
    }

    /// Create an upload oriented [`ThroughputClient`].
    pub fn upload(size: usize) -> Self {
        Self {
            connection: None,
            new_connection_type: None,
            headers: None,
            direction: Direction::Up(size),
            plain_http_mode: false,
        }
    }

    /// Send requests on the given [`EstablishedConnection`].
    pub fn with_connection(mut self, connection: Arc<RwLock<EstablishedConnection>>) -> Self {
        self.connection = Some(connection);
        self
    }

    /// Create a new connection for each request.
    pub fn new_connection(mut self, conn_type: ConnectionType) -> Self {
        self.new_connection_type = Some(conn_type);
        self
    }

    /// Set the headers for the upload or download request.
    pub fn headers(mut self, headers: HeaderMap<HeaderValue>) -> Self {
        self.headers = Some(headers);
        self
    }

    /// Enable plain HTTP mode (no TLS) specific header adjustments.
    ///
    /// In this mode additional headers (Accept, Connection, Content-Length)
    /// are injected to better emulate typical plain HTTP/1.1 client
    /// behavior. Leaving this disabled preserves the minimal header set used
    /// previously for HTTPS tests.
    pub fn plain_http_mode(mut self, yes: bool) -> Self { self.plain_http_mode = yes; self }

    /// Execute a download or upload request against the given [`Uri`].
    #[tracing::instrument(skip(self, network, time, shutdown))]
    pub fn send(
        self,
        uri: Uri,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<OneshotResult<InflightBody>> {
        let mut headers = self.headers.unwrap_or_default();

        if !headers.contains_key("User-Agent") {
            headers.insert("User-Agent", HeaderValue::from_static("mach/0.1.0"));
        }


    let host = uri.host().context("uri is missing a host")?.to_string();
        // Use correct default port based on scheme so HTTPS downloads go to 443.
        let default_port = match uri.scheme_str() { Some("https") => 443, _ => 80 };
        let host_with_port = format!("{}:{}", host, uri.port_u16().unwrap_or(default_port));

        let method = match self.direction {
            Direction::Down => "GET",
            Direction::Up(_) => "POST",
        };

        let (tx, rx) = oneshot_result();
        let mut events = None;

        let body: NqBody = match self.direction {
            Direction::Up(size) => {
                tracing::debug!("tracking upload body");
                let dummy_body = UploadBody::new(size);

                let (body, events_rx) =
                    CountingBody::new(dummy_body, Duration::from_millis(50), Arc::clone(&time));
                events = Some(events_rx);

                headers.insert("Content-Length", size.into());
                headers.insert("Content-Type", HeaderValue::from_static("text/plain"));

                body.boxed()
            }
            Direction::Down => {
                tracing::debug!("created empty download body");
                empty().boxed()
            }
        };

        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .body(body)?;

        tracing::debug!("created request");

        *request.headers_mut() = headers.clone();
        // Capture URI parts before mutable borrow
        let uri_host = request.uri().host().map(|s| s.to_string());
        let uri_port = request.uri().port_u16();
        let uri_scheme = request.uri().scheme_str().map(|s| s.to_string());
        let h = request.headers_mut();
        if let Some(host_hdr) = uri_host.as_deref() {
            let scheme = uri_scheme.as_deref();
            let default_port = match scheme { Some("https") => 443, _ => 80 };
            let need_port = uri_port.is_some() && uri_port.unwrap() != default_port;
            let host_value = if need_port { format!("{}:{}", host_hdr, uri_port.unwrap()) } else { host_hdr
.to_string() };
            if let Ok(val) = http::HeaderValue::from_str(&host_value) { h.insert("Host", val); }
        }
        if self.plain_http_mode {
            h.insert("Accept", http::HeaderValue::from_static("*/*"));
            h.insert("Connection", http::HeaderValue::from_static("keep-alive"));
            if let Direction::Up(size) = self.direction {
                if !h.contains_key("Content-Length") {
                    if let Ok(val) = http::HeaderValue::from_str(&size.to_string()) {
                        h.insert("Content-Length", val);
                    }
                }
            }
        }


        tokio::spawn(
            async move {
                let start = time.now();

                let connection = if let Some(connection) = self.connection {
                    connection
                } else if let Some(conn_type) = self.new_connection_type {
                    info!("creating new connection to {host_with_port}");

                    let addrs = network
                        .resolve(host_with_port)
                        .await
                        .context("unable to resolve host")?;

                    debug!("addrs: {addrs:?}");

                    network
                        .new_connection(start, addrs[0], host, conn_type)
                        .await
                        .context("creating new connection")?
                } else {
                    todo!()
                };

                let conn_timing = {
                    let conn = connection.read().await;
                    conn.timing()
                };

                debug!("connection used");
                let response_fut = network.send_request(connection.clone(), request);

                let mut response_body = match self.direction {
                    Direction::Up(_) => {
                        debug!("sending upload events");
                        if tx
                            .send(Ok(InflightBody {
                                connection: connection.clone(),
                                timing: Some(conn_timing),
                                events: events.expect("events were set above"),
                                start,
                                headers,
                            }))
                            .is_err()
                        {
                            error!("error sending upload events");
                        }

                        let (parts, incoming) = response_fut.await?.into_parts();
                        tracing::info!(status=?parts.status, headers=?parts.headers, "received response headers (upload)");

                        info!("upload response parts: {:?}", parts);

                        incoming.boxed()
                    }
                    Direction::Down => {
                        let (parts, incoming) = response_fut.await?.into_parts();

                        let (counting_body, events) = CountingBody::new(
                            incoming,
                            Duration::from_millis(100),
                            Arc::clone(&time),
                        );

                        debug!("sending download events");
                        if tx
                            .send(Ok(InflightBody {
                                connection: connection.clone(),
                                timing: Some(conn_timing),
                                start,
                                events,
                                headers: parts.headers,
                            }))
                            .is_err()
                        {
                            error!("error sending download events");
                        }

                        counting_body.boxed()
                    }
                };

                tokio::spawn(
                    async move {
                        // Consume the response body and keep the connection alive. Stop if we hit an error.
                        info!("waiting for response body");

                        loop {
                            select! {
                                Some(res) = response_body.frame() => if let Err(e) = res {
                                    error!("body closing: {e}");
                                    break;
                                },
                                _ = shutdown.cancelled() => break,
                            }
                        }
                    }
                    .in_current_span(),
                );

                Ok::<_, anyhow::Error>(())
            }
            .in_current_span(),
        );

        Ok(rx)
    }
}

/// A [`Client`] is a simple client which sends a request and returns a response.
///
/// The connection timing, e.g. TCP/TLS overhead, will be inserted into the response
/// if it exits.
#[derive(Default)]
pub struct Client {
    connection: Option<Arc<RwLock<EstablishedConnection>>>,
    new_connection_type: Option<ConnectionType>,
    headers: Option<HeaderMap>,
    method: Option<String>,
    plain_http_mode: bool,
}

impl Client {
    /// Send requests on the given [`EstablishedConnection`].
    pub fn with_connection(mut self, connection: Arc<RwLock<EstablishedConnection>>) -> Self {
        self.connection = Some(connection);
        self
    }

    /// Create a new connection for each request.
    pub fn new_connection(mut self, conn_type: ConnectionType) -> Self {
        self.new_connection_type = Some(conn_type);
        self
    }

    /// Set the headers for the upload or download request.
    pub fn headers(mut self, headers: HeaderMap<HeaderValue>) -> Self {
        self.headers = Some(headers);
        self
    }

    /// Set the method used by the client.
    pub fn method(mut self, method: &str) -> Self {
        self.method = Some(method.to_string());
        self
    }

    /// Enable or disable plain (non-TLS) HTTP mode specific header normalization.
    ///
    /// When set to `true`, the client will add extra headers such as
    /// `Accept: */*`, `Connection: keep-alive`, and (for sized bodies) a
    /// `Content-Length`. In TLS mode
    /// (`false`) these are omitted to preserve the original HTTPS behavior.
    pub fn plain_http_mode(mut self, yes: bool) -> Self { self.plain_http_mode = yes; self }

    /// Send a request to the given uri with the given body, timing how long it
    /// took.
    #[tracing::instrument(skip(self, body, network, time))]
    pub fn send<B>(
        self,
        uri: Uri,
        body: B,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
    ) -> anyhow::Result<OneshotResult<http::Response<Incoming>>>
    where
        B: Body<Data = Bytes, Error = Infallible> + Send + Sync + 'static,
    {
        let mut headers = self.headers.unwrap_or_default();

        if !headers.contains_key("User-Agent") {
            headers.insert("User-Agent", HeaderValue::from_static(MACH_USER_AGENT));
        }

        let host = uri.host().context("uri is missing a host")?.to_string();

        // Choose default port based on scheme (https -> 443, http -> 80, else 443)
        let default_port = match uri.scheme_str() {
            Some("https") => 443,
            Some("http") => 80,
            _ => 443,
        };
        let remote_addr = (host.as_str(), uri.port_u16().unwrap_or(default_port))
            .to_socket_addrs()?
            .next()
            .context("could not resolve large download url")?;

        let method: http::Method = self.method.as_deref().unwrap_or("GET").parse()?;

        // Add Content-Length if body reports an exact size and header not already set.
        if !headers.contains_key("Content-Length") {
            let size_hint = body.size_hint();
            if let Some(exact) = size_hint.exact() {
                if let Ok(v) = HeaderValue::from_str(&exact.to_string()) {
                    headers.insert("Content-Length", v);
                }
            }
        }

        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .body(body.boxed())?;

        *request.headers_mut() = headers.clone();
        // Always ensure Host header is present (required for HTTP/1.1). Only include port
        // if it differs from the default for the scheme.
        {
            let uri_host = request.uri().host().map(|s| s.to_string());
            if let Some(host_hdr) = uri_host.as_deref() {
                let uri_port = request.uri().port_u16();
                let scheme = request.uri().scheme_str();
                let default_port = match scheme { Some("https") => 443, Some("http") => 80, _ => 443 };
                let need_port = uri_port.is_some() && uri_port.unwrap() != default_port;
                let host_value = if need_port { format!("{}:{}", host_hdr, uri_port.unwrap()) } else { host_hdr.to_string() };
                let h = request.headers_mut();
                if !h.contains_key("Host") {
                    if let Ok(val) = http::HeaderValue::from_str(&host_value) { h.insert("Host", val); }
                }
            }
        }

        // Plain HTTP (no TLS) mode header normalization
        if self.plain_http_mode {
            let h = request.headers_mut();
            h.insert("Accept", http::HeaderValue::from_static("*/*"));
            h.insert("Connection", http::HeaderValue::from_static("keep-alive"));
        }

        debug!("sending request");

        let (tx, rx) = oneshot_result();
        tokio::spawn(
            async move {
                let start = time.now();

                let connection = if let Some(connection) = self.connection {
                    connection
                } else if let Some(conn_type) = self.new_connection_type {
                    info!("creating new connection");
                    network
                        .new_connection(start, remote_addr, host, conn_type)
                        .await?
                } else {
                    todo!()
                };

                // todo(fisher): fine-grained send timings for requests
                let mut response = network.send_request(connection.clone(), request).await?;

                let timing = {
                    let conn = connection.read().await;
                    conn.timing()
                };

                debug!(?connection, "connection used");

                response.extensions_mut().insert(timing);

                if tx.send(Ok(response)).is_err() {
                    error!("unable to send response");
                }

                Ok::<_, anyhow::Error>(())
            }
            .in_current_span(),
        );

        Ok(rx)
    }
}

/// Consumes body events until the body is finished and returns
/// the time at which the body finished.
pub async fn wait_for_finish(
    mut body_events: mpsc::UnboundedReceiver<BodyEvent>,
) -> anyhow::Result<FinishResult> {
    let mut body_total = 0;

    while let Some(event) = body_events.recv().await {
        match event {
            BodyEvent::ByteCount { total, .. } => body_total = total,
            BodyEvent::Finished { at } => {
                return Ok(FinishResult {
                    total: body_total,
                    finished_at: at,
                });
            }
        }
    }

    Err(anyhow::anyhow!("body did not finish"))
}

/// The result of [`wait_for_finish`]
#[derive(Debug)]
pub struct FinishResult {
    /// The total number of bytes seen by the body.
    pub total: usize,
    /// When the body finished.
    pub finished_at: Timestamp,
}
