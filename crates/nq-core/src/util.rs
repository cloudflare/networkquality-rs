use std::{future::Future, pin::Pin};

use http::Response;
use hyper::body::Incoming;
use tokio::{
    io::{AsyncRead, AsyncWrite},
};

/// A future that resolves to an http::Response.
pub type ResponseFuture = Pin<Box<dyn Future<Output = hyper::Result<Response<Incoming>>> + Send>>;

/// Trait representing a generic readable and writable byte stream.
/// Mostly copied from rxtx, thanks Ivan for the great abstractions :)
pub trait ByteStream: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static {}

impl<T> ByteStream for T where T: AsyncRead + AsyncWrite + Sync + Send + Unpin + 'static {}
