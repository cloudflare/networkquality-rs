// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use std::{future::Future, pin::Pin, task::Poll};

use http::Response;
use hyper::body::Incoming;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::oneshot,
};

/// A future that resolves to an http::Response.
pub type ResponseFuture = Pin<Box<dyn Future<Output = hyper::Result<Response<Incoming>>> + Send>>;

/// Create a oneshot channel with a custom implementation to make `Result`
/// handling more ergonomic.
pub fn oneshot_result<T>() -> (oneshot::Sender<crate::Result<T>>, OneshotResult<T>) {
    let (tx, rx) = oneshot::channel();

    (tx, OneshotResult { inner: rx })
}

/// An abstraction over a [`oneshot::Receiver`] of a result.
pub struct OneshotResult<T> {
    inner: oneshot::Receiver<crate::Result<T>>,
}

impl<T> Future for OneshotResult<T> {
    type Output = crate::Result<T>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().inner).poll(cx) {
            Poll::Ready(Ok(t)) => Poll::Ready(t),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Trait representing a generic readable and writable byte stream.
/// Mostly copied from rxtx, thanks Ivan for the great abstractions :)
pub trait ByteStream: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static {}

impl<T> ByteStream for T where T: AsyncRead + AsyncWrite + Sync + Send + Unpin + 'static {}
