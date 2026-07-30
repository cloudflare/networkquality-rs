// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

mod counting_body;
mod upload_body;

use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::RwLock;

use http::{HeaderMap, HeaderValue};
use http_body_util::{Empty, combinators::BoxBody};
use hyper::body::Bytes;
use tokio::sync::mpsc;

/// A simple boxed body.
pub type NqBody = BoxBody<Bytes, Infallible>;

/// Creates an empty body.
pub fn empty() -> Empty<Bytes> {
    Empty::new()
}

use crate::{EstablishedConnection, Timestamp, connection::ConnectionTiming};

pub use self::{
    counting_body::{BodyEvent, CountingBody},
    upload_body::UploadBody,
};

/// A body that is currently being sent or received.
pub struct InflightBody {
    /// When the request that produced this body was started.
    pub start: Timestamp,
    /// The connection the body is being transferred on. Shared so further
    /// requests can be sent on the same connection.
    pub connection: Arc<RwLock<EstablishedConnection>>,
    /// Connection setup timing, when this body created the connection.
    pub timing: Option<ConnectionTiming>,
    /// Byte-count and termination events for this body. The channel closing is
    /// itself meaningful: it signals the body was dropped.
    pub events: mpsc::UnboundedReceiver<BodyEvent>,
    /// Headers associated with the transfer.
    pub headers: HeaderMap<HeaderValue>,
}
