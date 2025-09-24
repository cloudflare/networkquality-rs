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
mod time;
mod upgraded;
mod util;

pub use crate::{
    body::{BodyEvent, CountingBody, NqBody},
    connection::{ConnectionManager, ConnectionTiming, ConnectionType, EstablishedConnection},
    network::Network,
    time::{Time, Timestamp, TokioTime},
    upgraded::ConnectUpgraded,
    util::{oneshot_result, OneshotResult, ResponseFuture},
};


pub use anyhow::Error;
pub use anyhow::Result;
