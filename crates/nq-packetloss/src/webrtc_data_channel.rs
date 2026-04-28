// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

use hyper::body::{Buf, Bytes};
use std::sync::{Arc, Weak};
use tokio::sync::mpsc::UnboundedSender;
use webrtc::{
    api::APIBuilder,
    data_channel::{RTCDataChannel, data_channel_init::RTCDataChannelInit},
    ice_transport::{
        ice_candidate::RTCIceCandidate, ice_protocol::RTCIceProtocol, ice_server::RTCIceServer,
    },
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        policy::ice_transport_policy::RTCIceTransportPolicy,
    },
};

use crate::TurnServerCreds;

/// A [`WebRTCDataChannel`] is a simple wrapper around a WebRTC peer connection and data channel used to
/// send UDP packets over a TURN data connection.
#[derive(Clone)]
pub struct WebRTCDataChannel {
    sender_peer_connection: Arc<RTCPeerConnection>,
    sender_data_channel: Arc<RTCDataChannel>,
    receiver_peer_connection: Arc<RTCPeerConnection>,
}

/// Message results reported to run_test loop
#[derive(Debug, PartialEq)]
pub enum DataChannelEvent {
    /// Sent when sender data channel has been opened
    OnOpenChannel,
    /// Sent for each message received and the contents
    OnReceivedMessage(usize),
    /// A failure occured
    ConnectionError(String),
}

impl WebRTCDataChannel {
    /// Create the peer connections and configure callback handlers
    pub async fn create_with_config(
        turn_server_uri: &String,
        turn_server_creds: &TurnServerCreds,
        event_tx: UnboundedSender<DataChannelEvent>,
    ) -> anyhow::Result<Self> {
        // WebRTC uses rustls "ring"
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Create the sender peer and data connection used to generate messages
        let (sender_peer_connection, sender_data_channel) =
            create_sender_data_connection(turn_server_uri, turn_server_creds, event_tx.clone())
                .await?;

        // Create the peer connection to receive the incoming messages
        let receiver_peer_connection =
            create_receiver_connection(turn_server_uri, turn_server_creds, event_tx.clone())
                .await?;

        add_ice_candidate_handler(
            sender_peer_connection.clone(),
            receiver_peer_connection.clone(),
        );
        add_ice_candidate_handler(
            receiver_peer_connection.clone(),
            sender_peer_connection.clone(),
        );

        add_peer_state_failed_handler(sender_peer_connection.clone(), event_tx.clone());
        add_peer_state_failed_handler(receiver_peer_connection.clone(), event_tx);

        Ok(Self {
            sender_peer_connection,
            sender_data_channel,
            receiver_peer_connection,
        })
    }

    /// Initiate the handshake to establish the data channel
    pub async fn establish_data_channel(&self) -> anyhow::Result<()> {
        let reqs = self.sender_peer_connection.create_offer(None).await?;
        self.sender_peer_connection
            .set_local_description(reqs.clone())
            .await?;
        self.receiver_peer_connection
            .set_remote_description(reqs)
            .await?;
        let resp = self.receiver_peer_connection.create_answer(None).await?;
        self.receiver_peer_connection
            .set_local_description(resp.clone())
            .await?;
        self.sender_peer_connection
            .set_remote_description(resp)
            .await?;

        Ok(())
    }

    /// Send the provided message over the data channel
    pub async fn send_message(&mut self, message: &[u8]) -> anyhow::Result<usize> {
        Ok(self
            .sender_data_channel
            .send(&Bytes::copy_from_slice(message))
            .await?)
    }
    /// Closes the peer connections. The data channels will be closed internally to the peer connection.
    pub async fn close_channel(&mut self) {
        let _ = self.sender_peer_connection.close().await;
        let _ = self.receiver_peer_connection.close().await;
    }
}

// The message sender creates a [`RTCPeerConnection`] followed by a [`RTCDataChannel`] used to send the
// test messages. The creation of the data channel is notified via the [`Sender`].
async fn create_sender_data_connection(
    turn_server_uri: &String,
    creds: &TurnServerCreds,
    event_tx: UnboundedSender<DataChannelEvent>,
) -> anyhow::Result<(Arc<RTCPeerConnection>, Arc<RTCDataChannel>)> {
    let peer_connection = Arc::new(create_peer_connection(turn_server_uri, creds).await?);

    // Since this is a packet loss test make sure retransmits are set to 0
    let options = Some(RTCDataChannelInit {
        ordered: Some(false),
        max_retransmits: Some(0u16),
        ..Default::default()
    });

    let data_channel = peer_connection
        .create_data_channel("channel", options)
        .await?;

    data_channel.on_open(Box::new(|| {
        Box::pin({
            async move {
                let _ = event_tx.send(DataChannelEvent::OnOpenChannel);
            }
        })
    }));

    Ok((peer_connection, data_channel))
}

/// The message receiver creates a [`RTCPeerConnection`] and opens the receiving end of the senders [`RTCDataChannel`].
/// The data channel is used to receive the sent messages and forward the contents via a [`Sender`].
async fn create_receiver_connection(
    turn_server_uri: &String,
    creds: &TurnServerCreds,
    event_tx: UnboundedSender<DataChannelEvent>,
) -> anyhow::Result<Arc<RTCPeerConnection>> {
    let peer_connection = Arc::new(create_peer_connection(turn_server_uri, creds).await?);

    peer_connection.on_data_channel(Box::new(move |data_channel| {
        Box::pin({
            let event_tx = event_tx.clone();
            async move {
                data_channel.on_message(Box::new(move |mut message| {
                    Box::pin({
                        let event_tx = event_tx.clone();
                        async move {
                            const USIZE_BYTES: usize = (usize::BITS / 8) as usize;
                            if message.data.len() == USIZE_BYTES {
                                let received_number = message.data.get_u64() as usize;
                                let _ = event_tx
                                    .send(DataChannelEvent::OnReceivedMessage(received_number));
                            } else {
                                tracing::warn!("message unexpected length! {}", message.data.len());
                            }
                        }
                    })
                }));
            }
        })
    }));

    Ok(peer_connection)
}

/// Allocates a new [`RTCPeerConnection`]
async fn create_peer_connection(
    turn_server_uri: &String,
    creds: &TurnServerCreds,
) -> anyhow::Result<RTCPeerConnection> {
    // Create API without any media options
    let api = APIBuilder::new().build();

    // Configure the ice server
    let ice_servers = vec![RTCIceServer {
        urls: vec![turn_server_uri.to_string()],
        username: creds.username.clone(),
        credential: creds.credential.clone(),
    }];

    // Configure the relay transport policy
    let config = RTCConfiguration {
        ice_servers,
        ice_transport_policy: RTCIceTransportPolicy::Relay,
        ..Default::default()
    };

    Ok(api.new_peer_connection(config).await?)
}

/// Sends a notification upon connection establishment failure
fn add_peer_state_failed_handler(
    peer_connection: Arc<RTCPeerConnection>,
    event_tx: UnboundedSender<DataChannelEvent>,
) {
    peer_connection.on_peer_connection_state_change(Box::new(
        move |state: RTCPeerConnectionState| {
            let connection_tx = event_tx.clone();

            Box::pin(async move {
                if state == RTCPeerConnectionState::Failed {
                    let _ = connection_tx.send(DataChannelEvent::ConnectionError(
                        "Data connection failed".to_owned(),
                    ));
                }
            })
        },
    ));
}

/// Adds a peer connection as an ice candidate to the second peer connection
fn add_ice_candidate_handler(
    first_connection: Arc<RTCPeerConnection>,
    second_connection: Arc<RTCPeerConnection>,
) {
    // Make sure the candidates match UDP protocol
    let maybe_first_connection = Arc::downgrade(&first_connection);
    second_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
        let maybe_first_connection = maybe_first_connection.clone();

        Box::pin(async move {
            if let Some(candidate) = candidate {
                if let Err(err) = add_ice_candidate(&candidate, &maybe_first_connection).await {
                    tracing::warn!(?err, "Failed to add sender ICE candidate")
                }
            }
        })
    }));
}

/// Ensure the candidate is UDP
async fn add_ice_candidate(
    candidate: &RTCIceCandidate,
    maybe_first_connection: &Weak<RTCPeerConnection>,
) -> anyhow::Result<()> {
    if candidate.protocol == RTCIceProtocol::Udp {
        let candidate = candidate.to_json()?;
        let first = maybe_first_connection
            .upgrade()
            .ok_or(anyhow::anyhow!("Expected a peer connection"))?;
        if let Err(err) = first.add_ice_candidate(candidate.clone()).await {
            tracing::warn!(?err, "Failed to add sender ICE candidate");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{net::SocketAddr, sync::Arc, time::Duration};

    use tokio::net::UdpSocket;
    use webrtc::{
        turn::{self, auth::AuthHandler},
        util,
    };

    use crate::{
        TurnServerCreds,
        webrtc_data_channel::{DataChannelEvent, WebRTCDataChannel},
    };

    // Canned password for the test TURN server
    pub(crate) struct TestAuthHandler {}

    impl AuthHandler for TestAuthHandler {
        fn auth_handle(
            &self,
            _username: &str,
            _realm: &str,
            _src_addr: SocketAddr,
        ) -> Result<Vec<u8>, turn::Error> {
            Ok(turn::auth::generate_auth_key(
                "username",
                "webrtc.rs",
                "password",
            ))
        }
    }

    // Test TURN server used by unit tests
    pub(crate) struct TestTurnServer {
        pub server: turn::server::Server,
        pub server_port: u16,
    }

    impl TestTurnServer {
        pub async fn start_turn_server() -> anyhow::Result<Self> {
            let sock_addr: SocketAddr = "127.0.0.1:0".parse()?;
            let udp_socket = Arc::new(UdpSocket::bind(sock_addr).await?);
            let server_port = udp_socket.local_addr()?.port();

            let server = turn::server::Server::new(turn::server::config::ServerConfig {
                realm: "webrtc.rs".to_owned(),
                auth_handler: Arc::new(TestAuthHandler {}),
                conn_configs: vec![turn::server::config::ConnConfig {
                    conn: udp_socket,
                    relay_addr_generator: Box::new(
                        turn::relay::relay_none::RelayAddressGeneratorNone {
                            address: "127.0.0.1".to_owned(),
                            net: Arc::new(util::vnet::net::Net::new(None)),
                        },
                    ),
                }],
                channel_bind_timeout: Duration::from_secs(0),
                alloc_close_notify: None,
            })
            .await?;

            Ok(Self {
                server,
                server_port,
            })
        }

        pub fn get_test_creds(&self) -> TurnServerCreds {
            TurnServerCreds {
                username: "username".to_owned(),
                credential: "password".to_owned(),
            }
        }

        pub async fn close(&self) -> anyhow::Result<()> {
            Ok(self.server.close().await?)
        }
    }

    #[tokio::test]
    async fn test_channel_open() -> anyhow::Result<()> {
        let server = TestTurnServer::start_turn_server().await?;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<DataChannelEvent>();
        let turn_server_creds = server.get_test_creds();
        let turn_server_uri = format!("turn:127.0.0.1:{}?transport=udp", server.server_port);
        let webrtc_data_channel = WebRTCDataChannel::create_with_config(
            &turn_server_uri,
            &turn_server_creds,
            event_tx.clone(),
        )
        .await?;

        // Establishing the data channel starts the flow of messages to the TURN server
        webrtc_data_channel.establish_data_channel().await?;
        let event = event_rx.recv().await;
        server.close().await?;

        assert_eq!(event.unwrap(), DataChannelEvent::OnOpenChannel);

        Ok(())
    }

    #[tokio::test]
    async fn test_bad_port_connection_error() -> anyhow::Result<()> {
        let server = TestTurnServer::start_turn_server().await?;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<DataChannelEvent>();
        let turn_server_creds = TurnServerCreds {
            username: "username".to_owned(),
            credential: "password".to_owned(),
        };
        let turn_server_uri = "turn:127.0.0.1:0?transport=udp".to_string();
        let webrtc_data_channel = WebRTCDataChannel::create_with_config(
            &turn_server_uri,
            &turn_server_creds,
            event_tx.clone(),
        )
        .await?;

        // Establishing the data channel starts the flow of messages to the TURN server
        webrtc_data_channel.establish_data_channel().await?;
        let event = event_rx.recv().await;
        server.close().await?;

        assert_eq!(
            event.unwrap(),
            DataChannelEvent::ConnectionError("Data connection failed".to_string())
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_bad_creds_connection_error() -> anyhow::Result<()> {
        let server = TestTurnServer::start_turn_server().await?;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<DataChannelEvent>();
        let turn_server_creds = TurnServerCreds {
            username: "bad_username".to_owned(),
            credential: "bad_password".to_owned(),
        };
        let turn_server_uri = format!("turn:127.0.0.1:{}?transport=udp", server.server_port);
        let webrtc_data_channel = WebRTCDataChannel::create_with_config(
            &turn_server_uri,
            &turn_server_creds,
            event_tx.clone(),
        )
        .await?;

        // Establishing the data channel starts the flow of messages to the TURN server
        webrtc_data_channel.establish_data_channel().await?;
        let event = event_rx.recv().await;
        server.close().await?;

        assert_eq!(
            event.unwrap(),
            DataChannelEvent::ConnectionError("Data connection failed".to_string())
        );

        Ok(())
    }
}
