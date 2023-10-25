//! An upgraded connection object which implements AsyncRead + AsyncWrite.
//!
//! Mostly copied from rxtx, thanks Ivan for the great abstractions :)

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub trait ConnectUpgradedInner: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl ConnectUpgradedInner for hyper_util::rt::TokioIo<hyper::upgrade::Upgraded> {}

/// A byte stream for CONNECT request upgrade communication.
pub struct ConnectUpgraded(Box<dyn ConnectUpgradedInner>);

// SAFETY: it's safe to assume that `Upgraded` is `Sync` as all the transports that we use
// within the library (`TcpStream`, `tokio_boring::SslStream`) are `Sync`.
unsafe impl Sync for ConnectUpgraded {}

impl ConnectUpgraded {
    /// Create a [`ConnectUpgrade`] from something that implements [`ConnectUpgradedInner`].
    pub fn new(upgraded: impl ConnectUpgradedInner) -> Self {
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
