use std::{net::SocketAddr, time::Duration};

use boring::ssl::{SslConnector, SslMethod, SslVerifyMode};
use tokio::{
    time::Instant,
};
use tracing::debug;

use crate::{util::ByteStream, ConnectionType};

pub type TlsStream = tokio_boring::SslStream<Box<dyn ByteStream>>;

#[tracing::instrument(skip(io))]
pub async fn tls_connection(
    started_at: Instant,
    addr: SocketAddr,
    domain: &str,
    conn_type: ConnectionType,
    io: impl ByteStream,
) -> anyhow::Result<(TlsTiming, TlsStream)> {
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
    let time_secure = started_at.elapsed();

    debug!(?time_secure, "created tls connection");

    let stats = TlsTiming {
        started_at,
        time_secure,
    };

    Ok((stats, ssl_stream))
}

pub struct TlsTiming {
    pub started_at: Instant,
    // pub time_connect: Duration,
    pub time_secure: Duration,
}
