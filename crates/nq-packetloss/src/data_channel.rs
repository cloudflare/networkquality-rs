// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Perform a simple round trip UDP packet test to a TURN server for the purpose of testing packet loss.
//! Avoids the overhead of using a full ICE negotiated DataChannel in favor of getting a simple UDP response.
//!

use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
    time::Duration,
};

use bytes::{Buf, Bytes};
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use nq_core::Network;
use tokio::net::UdpSocket;
use turnclient::{ChannelUsage, MessageFromTurnServer, MessageToTurnServer, TurnClient};

use crate::TurnServerCreds;

/// Used to indicate state and events during comminication with the TURN server
#[derive(Debug, PartialEq)]
pub enum DataChannelEvent {
    /// Sent when sender data channel has been opened
    OnOpenChannel,
    /// Sent for each message received and the contents
    OnReceivedMessage(usize),
    /// Sent when an event occurred that is treated as informational
    OnInfoMessage(String),
    /// A failure occured
    ConnectionError(String),
}

/// A [`TurnDataChannel`] is a simple turn client that uses a [`TurnClient`] to communicate with
/// a TURN server for the purpose of sending messages and returning the reply.
pub struct TurnDataChannel {
    /// Sink/Stream handles used to communicate configuration with TURN server
    turn_sink: SplitSink<TurnClient, MessageToTurnServer>,
    turn_stream: SplitStream<TurnClient>,
    /// The relay address return in the allocation message
    relay_address: Option<SocketAddr>,
    /// Used to send UDP packets to the relay address
    data_socket: Option<UdpSocket>,
    /// Stores the method of communicating test packets (STUN/UDP)
    channel_send_type: DataChannelSendType,
}

impl TurnDataChannel {
    // Message bytes used to identify method of sending packets to TURN server
    const SENTINEL_CHANNEL_BYTES: [u8; 4] = [10, 77, 15, 33];
    const SENTINEL_SOCKET_BYTES: [u8; 4] = [55, 21, 86, 2];
    const SENTINEL_MSG_LEN: usize = 4;
    const TEST_MSG_LEN: usize = (usize::BITS / 8) as usize;

    pub async fn create_with_config(
        turn_server_host_with_port: &str,
        turn_server_creds: &TurnServerCreds,
        network: Arc<dyn Network>,
    ) -> anyhow::Result<Self> {
        let addrs = network
            .resolve(turn_server_host_with_port.to_owned())
            .await?;
        let turn_server: SocketAddr = addrs[0];

        // Socket used by TurnClient
        let turn_socket = UdpSocket::bind(&get_bind_address_for_address(turn_server)).await?;

        let builder = turnclient::TurnClientBuilder {
            turn_server,
            username: turn_server_creds.username.clone(),
            password: turn_server_creds.credential.clone(),
            max_retries: 0,
            retry_interval: Duration::from_secs(5),
            refresh_interval: Duration::from_secs(30),
            software: Some("PacketLossTestTurnClient"),
            enable_mobility: false,
        };

        // Initialize TurnClient, does not send any packets yet.
        let (turn_sink, turn_stream): (
            SplitSink<TurnClient, MessageToTurnServer>,
            SplitStream<TurnClient>,
        ) = builder.build_and_send_request(turn_socket).split();

        Ok(Self {
            turn_sink,
            turn_stream,
            relay_address: None,
            data_socket: None,
            channel_send_type: DataChannelSendType::Undetermined,
        })
    }

    /// Called to handle communications to the TURN server. All traffic is generated and handled by this function.
    /// Must be invoked in a loop until the the test is finished.
    pub async fn wait_for_message(&mut self) -> anyhow::Result<DataChannelEvent> {
        // Polling the [`TurnClient`]` stream initiates traffic and returns any received messages
        match self.turn_stream.next().await {
            Some(message) => match message {
                Ok(from_server) => match from_server {
                    // Initial setup completed, send the AddPermission
                    MessageFromTurnServer::AllocationGranted {
                        relay_address,
                        mapped_address,
                        ..
                    } => {
                        self.handle_allocation_granted(relay_address, mapped_address)
                            .await
                    }

                    // Address permissions requested in AllocationGranted are indicated here
                    MessageFromTurnServer::PermissionCreated(socket_address) => {
                        self.handle_permission_created(socket_address).await
                    }

                    MessageFromTurnServer::PermissionNotCreated(socket_address) => {
                        Ok(DataChannelEvent::ConnectionError(
                            format!("Permission not created for {:?}", socket_address).to_owned(),
                        ))
                    }

                    // Test data and sentinel packets are handled here
                    MessageFromTurnServer::RecvFrom(socket_address, data) => {
                        self.handle_recv_packet(socket_address, data)
                    }
                    _ => Ok(DataChannelEvent::OnInfoMessage(
                        format!("{:?}", from_server).to_owned(),
                    )),
                },
                Err(err) => Err(err),
            },
            None => Ok(DataChannelEvent::ConnectionError(
                "Unexpected stream result".to_owned(),
            )),
        }
    }

    /// Allocate success is the initial response providing the relay and mapped addresses. Use these addresses
    /// to request permissions to communicate back to the STUN server.
    async fn handle_allocation_granted(
        &mut self,
        relay_address: SocketAddr,
        mapped_address: SocketAddr,
    ) -> anyhow::Result<DataChannelEvent> {
        // Save the relay_address for the round trip test
        self.relay_address = Some(relay_address);
        self.data_socket =
            Some(UdpSocket::bind(get_bind_address_for_address(relay_address)).await?);

        // Request permissions for both the relay and mapped addresses.
        self.turn_sink
            .send(MessageToTurnServer::AddPermission(
                relay_address,
                ChannelUsage::WithChannel,
            ))
            .await?;
        self.turn_sink
            .send(MessageToTurnServer::AddPermission(
                mapped_address,
                ChannelUsage::WithChannel,
            ))
            .await?;

        Ok(DataChannelEvent::OnInfoMessage(
            format!("Allocation granted for {:?}", relay_address).to_owned(),
        ))
    }

    /// TURN server indicates that a requested address has permission. If the indicated address
    /// is the relay address then it's time to send the sentinal packet to start the data channel.
    /// Performs the "UDP hole punching" part of the exchange. This is done by performing a race
    /// of STUN and UDP packets to the relay address. The packets contain a unique message which
    /// the TURN server will echo back. TURN servers may respond to one or the other, or both.
    /// A successful response identifies how to communicate with the TURN server for the remainder
    /// of the packet loss test.
    async fn handle_permission_created(
        &mut self,
        socket_address: SocketAddr,
    ) -> anyhow::Result<DataChannelEvent> {
        if let (Some(relay_address), Some(data_socket)) = (self.relay_address, &self.data_socket) {
            if socket_address == relay_address {
                // Send a message via STUN to open up the channel
                self.turn_sink
                    .send(MessageToTurnServer::SendTo(
                        relay_address,
                        Self::SENTINEL_CHANNEL_BYTES.to_vec(),
                    ))
                    .await?;

                // Send a UDP packet to open up the channel
                data_socket
                    .send_to(&Self::SENTINEL_SOCKET_BYTES, relay_address)
                    .await?;
            }
        }

        Ok(DataChannelEvent::OnInfoMessage(
            format!("Permission created for {:?}", socket_address).to_owned(),
        ))
    }

    /// Handle a data packet from the TURN server. Both initial sentinel packets and test data
    /// packets are handled here. The sentinel packet is indicated as an OnOpenChannel event
    /// to inform the test harness that it's safe to begin the packet loss test.
    fn handle_recv_packet(
        &mut self,
        socket_address: SocketAddr,
        data: Vec<u8>,
    ) -> anyhow::Result<DataChannelEvent> {
        match (
            data.len(),
            self.channel_send_type == DataChannelSendType::Undetermined,
        ) {
            // Handle the first sentinal message response from the TURN server. This
            // establishes the send type used for the follow on data packets. The TURN server
            // may reply to both so this picks the first one processed.
            // DataChannelSendType::UseChannel results in using the channel, aka STUN packets
            // DataChannelSendType::UseSocket results in using a UDP socket
            (Self::SENTINEL_MSG_LEN, true) => {
                if data == Self::SENTINEL_CHANNEL_BYTES {
                    tracing::trace!(
                        "Sentinel received, channel type, ready for test {:?}",
                        socket_address
                    );
                    self.channel_send_type = DataChannelSendType::UseChannel;
                    return Ok(DataChannelEvent::OnOpenChannel);
                } else if data == Self::SENTINEL_SOCKET_BYTES {
                    tracing::trace!(
                        "Sentinel received, socket type, ready for test {:?}",
                        socket_address
                    );
                    self.channel_send_type = DataChannelSendType::UseSocket;
                    return Ok(DataChannelEvent::OnOpenChannel);
                }
                Ok(DataChannelEvent::ConnectionError(
                    "Received unexpected bytes".to_owned(),
                ))
            }

            // If both STUN and UDP are responded to, just discard the second message with
            // an info response which will be ignored
            (Self::SENTINEL_MSG_LEN, false) => Ok(DataChannelEvent::OnInfoMessage(
                "Unused sentinal message".to_owned(),
            )),

            // Handle the bulk test packets that are used to test for loss
            (Self::TEST_MSG_LEN, false) => {
                let mut bytes: Bytes = data.clone().into();
                let received_number = bytes.get_u64() as usize;
                Ok(DataChannelEvent::OnReceivedMessage(received_number))
            }

            _ => Ok(DataChannelEvent::ConnectionError(
                "Unexpected message".to_owned(),
            )),
        }
    }

    /// Send a message, in this case a number, to be echoed back by the TURN server.
    pub async fn send_message(&mut self, message: usize) -> anyhow::Result<usize> {
        let (Some(relay_address), Some(data_socket)) = (self.relay_address, &self.data_socket)
        else {
            return Err(anyhow::anyhow!("Sending before ready"));
        };

        match self.channel_send_type {
            DataChannelSendType::UseChannel => {
                self.turn_sink
                    .send(MessageToTurnServer::SendTo(
                        relay_address,
                        message.to_be_bytes().to_vec(),
                    ))
                    .await?;
                Ok(message.to_be_bytes().len())
            }
            DataChannelSendType::UseSocket => Ok(data_socket
                .send_to(&message.to_be_bytes(), relay_address)
                .await?),
            DataChannelSendType::Undetermined => Err(anyhow::anyhow!("Sending before ready")),
        }
    }

    /// Close down the data channel to the TURN server
    pub async fn close_connection(&mut self) -> anyhow::Result<()> {
        self.turn_sink.close().await
    }
}

fn get_bind_address_for_address(socket_address: SocketAddr) -> SocketAddr {
    match socket_address {
        std::net::SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
        std::net::SocketAddr::V6(_) => {
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0))
        }
    }
}

// Describes the send_message method for the data messages to the TURN server
#[derive(PartialEq, Debug)]
enum DataChannelSendType {
    UseChannel,
    UseSocket,
    Undetermined,
}

#[cfg(test)]
pub(crate) mod tests {
    use nq_core::{Time, TokioTime};
    use nq_tokio_network::TokioNetwork;
    use std::{net::SocketAddr, sync::Arc, time::Duration};
    use tokio::net::UdpSocket;
    use tokio_util::sync::CancellationToken;
    use webrtc::{
        turn::{self, auth::AuthHandler},
        util,
    };

    use crate::{
        data_channel::{DataChannelEvent, TurnDataChannel},
        TurnServerCreds,
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

    // Test WebRTC TURN server used by unit tests
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

    // Test a simple open channel to a local WebRTC server
    #[tokio::test]
    async fn test_channel_open() -> anyhow::Result<()> {
        let shutdown = CancellationToken::new();
        let time = Arc::new(TokioTime::new());
        let network = Arc::new(TokioNetwork::new(
            Arc::clone(&time) as Arc<dyn Time>,
            shutdown.clone(),
        ));
        let server = TestTurnServer::start_turn_server().await?;
        let turn_server_creds = server.get_test_creds();
        let turn_server_host_with_port = format!("127.0.0.1:{}", server.server_port);
        let mut turn_data_channel = TurnDataChannel::create_with_config(
            &turn_server_host_with_port,
            &turn_server_creds,
            network,
        )
        .await?;

        let mut connected = false;
        loop {
            tokio::select! {
                Ok(event) = turn_data_channel.wait_for_message() => {
                    if event == DataChannelEvent::OnOpenChannel {
                        connected = true;
                        break;
                    }
                }

                () = tokio::time::sleep(Duration::from_secs(1)) => {
                    println!("Timed out");
                    break;
                }
            }
        }

        server.close().await?;
        assert!(connected);

        Ok(())
    }

    // Test sending a single data message to a local WebRTC server
    #[tokio::test]
    async fn test_send_message_success() -> anyhow::Result<()> {
        let shutdown = CancellationToken::new();
        let time = Arc::new(TokioTime::new());
        let network = Arc::new(TokioNetwork::new(
            Arc::clone(&time) as Arc<dyn Time>,
            shutdown.clone(),
        ));
        let server = TestTurnServer::start_turn_server().await?;
        let turn_server_creds = server.get_test_creds();
        let turn_server_host_with_port = format!("127.0.0.1:{}", server.server_port);
        let mut turn_data_channel = TurnDataChannel::create_with_config(
            &turn_server_host_with_port,
            &turn_server_creds,
            network,
        )
        .await?;

        let mut received_message = false;
        loop {
            tokio::select! {
                Ok(event) = turn_data_channel.wait_for_message() => {
                    println!("Event: {:?}", event);
                    match event {
                        DataChannelEvent::OnOpenChannel => {
                            let _ = turn_data_channel.send_message(1).await;
                        }
                        DataChannelEvent::OnReceivedMessage(message_num) => {
                            received_message = true;
                            assert!(message_num == 1);
                            break;
                        }
                        DataChannelEvent::ConnectionError(err) => panic!("Unexpected connection error {}", err),
                        DataChannelEvent::OnInfoMessage(info) => println!("Info: {}", info),
                    }
                }

                () = tokio::time::sleep(Duration::from_secs(10)) => {
                    println!("Timed out");
                    break;
                }
            }
        }

        server.close().await?;
        assert!(received_message);

        Ok(())
    }
}
