// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! The core abstraction for networkquality.
//!
//! Defines the main traits:
//! - [`Network`]: for abstracting over connections and http requests.
//! - [`Time`]: for abstracting over different implementations of time.

#![deny(missing_docs)]

mod body;
pub mod client;
mod connection;
mod network;
mod scoped_headers;
mod time;
mod util;

pub use self::{
    body::{BodyEvent, InflightBody, NqBody},
    connection::{
        ConnectionManager, ConnectionTiming, ConnectionType, EstablishedConnection,
        set_insecure_tls,
    },
    network::Network,
    scoped_headers::ScopedHeaders,
    time::{Time, Timestamp, TokioTime},
    util::{OneshotResult, ResponseFuture, oneshot_result},
};

pub use anyhow::Result;
