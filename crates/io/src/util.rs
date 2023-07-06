//! Mostly copied from rxtx, thanks Ivan for the great abstractions :)

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use http::Response;
use hyper::body::Incoming;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A future that resolves to an http::Response.
pub type ResponseFuture = Pin<Box<dyn Future<Output = hyper::Result<Response<Incoming>>> + Send>>;

/// Resolves to a Result of some `T`. Many non-async functions return this.
pub type OneshotResult<T> = tokio::sync::oneshot::Receiver<anyhow::Result<T>>;

/// Trait representing a generic readable and writable byte stream.
pub trait ByteStream: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static {}

impl<T> ByteStream for T where T: AsyncRead + AsyncWrite + Sync + Send + Unpin + 'static {}

pub(crate) trait ConnectUpgradedInner:
    AsyncRead + AsyncWrite + Send + Unpin + 'static
{
}

impl ConnectUpgradedInner for hyper::upgrade::Upgraded {}

/// A byte stream for CONNECT request upgrade communication.
pub struct ConnectUpgraded(Box<dyn ConnectUpgradedInner>);

// SAFETY: it's safe to assume that `Upgraded` is `Sync` as all the transports that we use
// within the library (`TcpStream`, `UnixStream`, `hyper_rustls::MaybeHttpsStream`,
// `tokio_boring::SslStream`) are `Sync`.
unsafe impl Sync for ConnectUpgraded {}

impl ConnectUpgraded {
    pub(crate) fn new(upgraded: impl ConnectUpgradedInner) -> Self {
        Self(Box::new(upgraded))
    }
}

impl AsyncRead for ConnectUpgraded {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.get_mut().0), cx, buf)
    }
}

impl AsyncWrite for ConnectUpgraded {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.get_mut().0), cx, buf)
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().0), cx)
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.get_mut().0), cx)
    }
}
