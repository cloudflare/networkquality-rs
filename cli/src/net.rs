use std::net::ToSocketAddrs;

use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
// use nq_core::{Responsiveness, ResponsivenessConfig, Speedtest};
use nq_io::{Network, NewConnectionArgs, OSNetwork};
use tokio::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url: http::Uri = std::env::args().nth(1).unwrap().parse()?;

    let domain = url.host().unwrap();
    let host = url.host().unwrap();
    let port = url.port().map(|p| p.as_u16()).unwrap_or(443);
    let remote_addr = (host, port).to_socket_addrs()?.next().unwrap();

    tracing_subscriber::fmt::init();

    let network = OSNetwork::default();

    let connection = network
        .new_connection(NewConnectionArgs::new(
            Instant::now(),
            remote_addr,
            domain.to_string(),
            nq_io::ConnectionType::H2,
        ))
        .await??;

    let start = Instant::now();
    let total = tokio::spawn(async move {
        let (_, mut body) = network
            .send_request(
                start,
                connection.id,
                http::Request::builder()
                    .uri(url)
                    .method("GET")
                    .body(Empty::<Bytes>::new())
                    .unwrap()
                    .map(|b| b.boxed()),
            )
            .await
            .unwrap()
            .unwrap()
            .await
            .unwrap()
            .into_parts();

        let mut total = 0;
        while let Some(Ok(frame)) = body.frame().await {
            if let Some(data) = frame.data_ref() {
                total += data.len();
            }
        }

        total
    })
    .await?;

    let duration = start.elapsed();
    println!("total: {total}");
    println!("time:  {:?}", duration);
    println!("bps:   {:.2?}", total as f64 * 8.0 / duration.as_secs_f64());

    Ok(())
}
