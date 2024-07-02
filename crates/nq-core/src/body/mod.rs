// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

mod counting_body;
mod upload_body;

use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::RwLock;

use http::{HeaderMap, HeaderValue};
use http_body_util::{combinators::BoxBody, Empty};
use hyper::body::Bytes;
use tokio::sync::mpsc;

/// A simple boxed body.
pub type NqBody = BoxBody<Bytes, Infallible>;

/// Creates an empty body.
pub fn empty() -> Empty<Bytes> {
    Empty::new()
}

use crate::{connection::ConnectionTiming, EstablishedConnection, Timestamp};

pub use self::{
    counting_body::{BodyEvent, CountingBody},
    upload_body::UploadBody,
};

/// A body that is currently being sent or received.
pub struct InflightBody {
    pub start: Timestamp,
    pub connection: Arc<RwLock<EstablishedConnection>>,
    pub timing: Option<ConnectionTiming>,
    pub events: mpsc::UnboundedReceiver<BodyEvent>,
    pub headers: HeaderMap<HeaderValue>,
}
