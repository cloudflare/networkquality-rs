use std::{convert::Infallible, net::ToSocketAddrs, sync::Arc, time::Duration};

use anyhow::Context as AnyhowContext;
use http::{HeaderMap, HeaderValue, Uri};
use http_body_util::{combinators::BoxBody, BodyExt, Empty};
use hyper::body::{Body, Bytes, Incoming};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use tracing::{error, info, Instrument};

use crate::{
    body::{CountingBody, DummyBody, InflightBody},
    BodyEvent, ConnectionId, ConnectionType, Network, NewConnectionArgs, OneshotResult,
};

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
#[derive(Default)]
pub struct ThroughputClient {
    conn_id: Option<ConnectionId>,
    new_connection_type: Option<ConnectionType>,
    headers: Option<HeaderMap>,
}

impl ThroughputClient {
    pub fn with_connection(mut self, conn_id: ConnectionId) -> Self {
        self.conn_id = Some(conn_id);
        self
    }

    pub fn new_connection(mut self, conn_type: ConnectionType) -> Self {
        self.new_connection_type = Some(conn_type);
        self
    }

    pub fn headers(mut self, headers: HeaderMap<HeaderValue>) -> Self {
        self.headers = Some(headers);
        self
    }

    #[tracing::instrument(skip(self, network))]
    pub fn send(
        self,
        network: Arc<dyn Network>,
        direction: Direction,
        uri: Uri,
    ) -> anyhow::Result<OneshotResult<InflightBody>> {
        let mut headers = self.headers.unwrap_or_default();

        if !headers.contains_key("User-Agent") {
            headers.insert("User-Agent", HeaderValue::from_static("mach/0.1.0"));
        }

        let host = uri.host().context("uri is missing a host")?.to_string();

        let remote_addr = (host.as_str(), uri.port_u16().unwrap_or(443))
            .to_socket_addrs()?
            .next()
            .context("could not resolve large download url")?;

        let method = match direction {
            Direction::Down => "GET",
            Direction::Up(_) => "POST",
        };

        let (tx, rx) = oneshot::channel();
        let (events_tx, events_rx) = mpsc::channel(1024);

        let body: BoxBody<Bytes, Infallible> = match direction {
            Direction::Up(size) => {
                let dummy_body = DummyBody::new(size);

                let mut body = CountingBody::new(dummy_body, Duration::from_millis(100));
                body.set_sender(events_tx);

                headers.insert("Content-Size", size.into());
                headers.insert("Content-Type", HeaderValue::from_static("text/plain"));

                body.boxed()
            }
            Direction::Down => Empty::<Bytes>::new().boxed(),
        };

        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .body(body)?;

        *request.headers_mut() = headers.clone();

        info!("sending request");

        tokio::spawn(
            async move {
                let start = Instant::now();

                let (conn_id, connection_timing) = if let Some(conn_id) = self.conn_id {
                    (conn_id, None)
                } else if let Some(conn_type) = self.new_connection_type {
                    info!("creating new connection");
                    let connection = network
                        .new_connection(NewConnectionArgs::new(start, remote_addr, host, conn_type))
                        .await??;

                    (connection.id, Some(connection.timing))
                } else {
                    todo!()
                };

                info!(?conn_id, "connection used");
                let response_fut = network.send_request(start, conn_id, request).await??;

                let mut response_body = match direction {
                    Direction::Up(_) => {
                        info!("sending upload events");
                        if tx
                            .send(Ok(InflightBody {
                                conn_id,
                                connection_timing,
                                events: events_rx,
                                start,
                                headers,
                            }))
                            .is_err()
                        {
                            // TODO(mark error)
                        }

                        let (_, incoming) = response_fut.await?.into_parts();
                        incoming.boxed()
                    }
                    Direction::Down => {
                        let (parts, incoming) = response_fut.await?.into_parts();

                        let mut counting_body =
                            CountingBody::new(incoming, Duration::from_millis(100));
                        let events = counting_body.subscribe();

                        info!("sending download events");
                        if tx
                            .send(Ok(InflightBody {
                                conn_id,
                                connection_timing,
                                start,
                                events,
                                headers: parts.headers,
                            }))
                            .is_err()
                        {
                            // TODO(mark error)
                        }

                        counting_body.boxed()
                    }
                };

                tokio::spawn(async move {
                    // Consume the response body and keep the connection alive:
                    info!("waiting for response body");
                    while response_body.frame().await.is_some() {}
                });

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
    conn_id: Option<ConnectionId>,
    new_connection_type: Option<ConnectionType>,
    headers: Option<HeaderMap>,
    method: Option<String>,
}

impl Client {
    pub fn with_connection(mut self, conn_id: ConnectionId) -> Self {
        self.conn_id = Some(conn_id);
        self
    }

    pub fn new_connection(mut self, conn_type: ConnectionType) -> Self {
        self.new_connection_type = Some(conn_type);
        self
    }

    pub fn headers(mut self, headers: HeaderMap<HeaderValue>) -> Self {
        self.headers = Some(headers);
        self
    }

    pub fn method(mut self, method: &str) -> Self {
        self.method = Some(method.to_string());
        self
    }

    #[tracing::instrument(skip(self, network, body))]
    pub fn send<B>(
        self,
        network: Arc<dyn Network>,
        uri: Uri,
        body: B,
    ) -> anyhow::Result<OneshotResult<http::Response<Incoming>>>
    where
        B: Body<Data = Bytes, Error = Infallible> + Send + Sync + 'static,
    {
        let mut headers = self.headers.unwrap_or_default();

        if !headers.contains_key("User-Agent") {
            headers.insert("User-Agent", HeaderValue::from_static("mach/0.1.0"));
        }

        let host = uri.host().context("uri is missing a host")?.to_string();

        let remote_addr = (host.as_str(), uri.port_u16().unwrap_or(443))
            .to_socket_addrs()?
            .next()
            .context("could not resolve large download url")?;

        let method: http::Method = self.method.as_deref().unwrap_or("GET").parse()?;

        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .body(body.boxed())?;

        *request.headers_mut() = headers.clone();

        info!("sending request");

        let (tx, rx) = oneshot::channel();
        tokio::spawn(
            async move {
                let start = Instant::now();

                let (conn_id, connection_timing) = if let Some(conn_id) = self.conn_id {
                    (conn_id, None)
                } else if let Some(conn_type) = self.new_connection_type {
                    info!("creating new connection");
                    let connection = network
                        .new_connection(NewConnectionArgs::new(start, remote_addr, host, conn_type))
                        .await??;

                    (connection.id, Some(connection.timing))
                } else {
                    todo!()
                };

                info!(?conn_id, "connection used");
                let mut response = network
                    .send_request(start, conn_id, request)
                    .await??
                    .await?;

                if let Some(timing) = connection_timing {
                    response.extensions_mut().insert(timing);
                }

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

pub async fn wait_for_finish(
    mut body_events: mpsc::Receiver<BodyEvent>,
) -> anyhow::Result<Instant> {
    while let Some(event) = body_events.recv().await {
        if let BodyEvent::Finished { at } = event {
            return Ok(at);
        }
    }

    Err(anyhow::anyhow!("body did not finish"))
}
