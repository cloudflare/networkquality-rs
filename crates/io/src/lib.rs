mod body;
pub mod client;
mod connection;
mod network;
mod tls;

pub(crate) mod util;

pub use crate::body::BodyEvent;
pub use crate::connection::ConnectionType;
pub use crate::network::{
    os::OSNetwork,
    proxied::{ProxyConfig, ProxyNetwork},
    ConnectionEstablished, ConnectionId, Network, NewConnectionArgs,
};
pub use crate::util::OneshotResult;
