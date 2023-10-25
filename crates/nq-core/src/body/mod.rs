mod counting_body;
mod upload_body;

use std::convert::Infallible;

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

use crate::{connection::ConnectionTiming, ConnectionId, Timestamp};

pub use self::{
    counting_body::{BodyEvent, CountingBody},
    upload_body::UploadBody,
};

/// A body that is currently being sent or received.
pub struct InflightBody {
    pub start: Timestamp,
    pub conn_id: ConnectionId,
    pub timing: Option<ConnectionTiming>,
    pub events: mpsc::Receiver<BodyEvent>,
    pub headers: HeaderMap<HeaderValue>,
}
