// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! This crate is a Rust implementation the Packet Loss measurement as performed by the javascript project
//! at https://github.com/cloudflare/speedtest.
//!
//! As stated by that project, Packet loss is measured by submitting a set of UDP packets to a WebRTC TURN server
//! in a round-trip fashion, and determining how many packets do not arrive.

mod webrtc_data_channel;

use nq_core::{
    client::Direction,
    ConnectionType, Network, Time, TokioTime,
};
use nq_load_generator::{LoadConfig, LoadGenerator, LoadedConnection};
use nq_tokio_network::TokioNetwork;
use serde::{Deserialize, Serialize};
use std::{cmp::min, collections::HashMap, fmt::Display, sync::Arc, time::Duration};
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use url::Url;
use webrtc_data_channel::{DataChannelEvent, WebRTCDataChannel};

#[derive(Clone, Debug)]
pub struct PacketLossConfig {
    /// The target TURN server URI to send UDP packets
    pub turn_server_uri: String,
    /// The URL to send the request to for TURN server credentials
    pub turn_cred_request_url: Url,
    /// Total number of messages/packets to send
    pub num_packets: usize,
    /// Total number of messages to send in a batch before waiting
    pub batch_size: usize,
    /// Time to wait between batch sends
    pub batch_wait_time: Duration,
    /// Time to wait for receiving messages after all messages have been sent
    pub response_wait_time: Duration,
    /// Download URL to use for for the [`LoadGenerator`]
    pub download_url: Url,
    /// Upload URL to use for for the [`LoadGenerator`]
    pub upload_url: Url,
}

impl Default for PacketLossConfig {
    fn default() -> Self {
        Self {
            turn_server_uri: "turn:turn.speed.cloudflare.com:50000?transport=udp".to_owned(),
            turn_cred_request_url: "https://speed.cloudflare.com/turn-creds".parse().unwrap(),
            num_packets: 1000,
            batch_size: 10,
            batch_wait_time: Duration::from_millis(10),
            response_wait_time: Duration::from_millis(3000),
            download_url: "https://h3.speed.cloudflare.com/__down?bytes=10000000000"
                .parse()
                .unwrap(),
            upload_url: "https://h3.speed.cloudflare.com/__up"
                .parse()
                .unwrap(),
        }
    }
}

impl PacketLossConfig {
    pub fn load_config(&self) -> LoadConfig {
        LoadConfig {
            headers: HashMap::default(),
            download_url: self.download_url.clone(),
            upload_url: self.upload_url.clone(),
            upload_size: 4_000_000_000, // 4 GB
        }
    }
}

/// A [`PacketLoss`] is a test harness around a WebRTC connection that sends UDP packets to a
/// TURN server and count the returned packets to calculate loss ratio.
pub struct PacketLoss {
    config: Arc<PacketLossConfig>,
    load_generator: LoadGenerator,
    /// Used to track the messages received
    /// A sent message is stored as false until a response is received setting it to true
    message_tracker: Arc<RwLock<Vec<bool>>>,
}

impl PacketLoss {
    pub fn new_with_config(config: PacketLossConfig) -> anyhow::Result<Self> {
        let load_generator = LoadGenerator::new(config.load_config())?;
        let message_vec = vec![false; config.num_packets];
        let message_tracker: Arc<RwLock<Vec<bool>>> = Arc::new(RwLock::new(message_vec));
        Ok(Self {
            config: Arc::new(config),
            load_generator,
            message_tracker,
        })
    }

    // Bi-directional load generators
    fn add_load_generators(
        &self,
        packet_event_tx: mpsc::Sender<PacketLossEvent>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<()> {
        // Start generating load on the network in both directions
        let time = Arc::new(TokioTime::new()) as Arc<dyn Time>;
        let network =
            Arc::new(TokioNetwork::new(Arc::clone(&time), shutdown.clone())) as Arc<dyn Network>;

        self.new_load_generating_connection(
            packet_event_tx.clone(),
            Direction::Down,
            network.clone(),
            time.clone(),
            shutdown.clone(),
        )?;
        self.new_load_generating_connection(
            packet_event_tx.clone(),
            Direction::Up(4_000_000_000),
            network,
            time,
            shutdown.clone(),
        )?;
        Ok(())
    }

    /// Run the Packet Loss test against the configured TURN server
    pub async fn run_test(
        mut self,
        turn_server_creds: TurnServerCreds,
        shutdown: CancellationToken,
    ) -> anyhow::Result<PacketLossResult> {
        let (webrtc_event_tx, mut webrtc_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<DataChannelEvent>();
        let (packet_event_tx, mut packet_event_rx) =
            tokio::sync::mpsc::channel::<PacketLossEvent>(3);

        #[cfg(not(test))]
        self.add_load_generators(packet_event_tx.clone(), shutdown.clone())?;

        let mut webrtc_data_channel = WebRTCDataChannel::create_with_config(
            &self.config.turn_server_uri,
            &turn_server_creds,
            webrtc_event_tx.clone(),
        )
        .await?;

        // Establishing the data channel starts the flow of messages to the TURN server
        webrtc_data_channel.establish_data_channel().await?;

        loop {
            tokio::select! {
                Some(event) = webrtc_event_rx.recv() => {
                    match event {
                        // Received when the WebRTC data channel has been established. Inidicates the data channel is ready for traffic
                        DataChannelEvent::OnOpenChannel => {
                            self.send_messages(webrtc_data_channel.clone(), packet_event_tx.clone(), shutdown.clone());
                        }

                        // Each message that is received on the data channel is notified and handled here
                        DataChannelEvent::OnReceivedMessage(message) => {
                            self.message_tracker
                                .write()
                                .await
                                .insert(message, true);
                        }

                        // A failure occured during setup of the data channel
                        DataChannelEvent::ConnectionError(err) => {
                            tracing::warn!("Failed to complete packet loss test {}", err);
                            break;
                        }
                    }
                }

                Some(event) = packet_event_rx.recv() => {
                    match event {
                        // All of the messages / UDP packets have been submitted and the wait timeout has completed.
                        PacketLossEvent::AllMessagesSent => {
                            break;
                        }

                        // Successfully created a load generator
                        PacketLossEvent::NewLoadedConnection(connection) => {
                            self.load_generator.push(connection);
                        }

                        // Failed to create a load generator
                        PacketLossEvent::Error(err) => {
                            tracing::warn!("Failed to complete packet loss test {}", err);
                            break;
                        }
                    }
                }

                _ = shutdown.cancelled() => {
                    tracing::debug!("Shutdown requested");
                    break;
                }
            }
        }

        // stop all on-going loads generators
        let mut loads = self.load_generator.into_connections();
        loads.iter_mut().for_each(|load| load.stop());

        webrtc_data_channel.close_channel().await;

        // Only count the messages if they are flagged as true (i.e. received)
        let num_messages = self
            .message_tracker
            .read()
            .await
            .iter()
            .filter(|val| **val)
            .count();
        let loss_ratio = (((self.config.num_packets - num_messages) as f64
            / self.config.num_packets as f64)
            * 10_000.0)
            .trunc()
            / 100.0;
        Ok(PacketLossResult {
            num_messages,
            loss_ratio,
        })
    }

    /// Write messages to the data channel of an incrementing sequence of numbers used to identify unique messages/packets.
    fn send_messages(
        &self,
        mut webrtc_data_channel: WebRTCDataChannel,
        send_message_tx: mpsc::Sender<PacketLossEvent>,
        shutdown: CancellationToken,
    ) {
        let config = self.config.clone();
        tokio::spawn(async move {
            let mut message_count = 0;
            loop {
                let batch_start = message_count;
                let batch_count = if config.batch_size == 0 {
                    config.num_packets
                } else {
                    min(message_count + config.batch_size, config.num_packets)
                };
                // Send messages in batches of a configured size
                for i in batch_start..batch_count {
                    // Send the message to the TURN server
                    if let Err(err) = webrtc_data_channel.send_message(&i.to_be_bytes()).await {
                        tracing::warn!("Send message failed: {}", err);
                    }
                    message_count += 1;
                }

                // Sleep for the configured amount of time then send noficiation that all messages have been sent
                if (message_count + 1) >= config.num_packets {
                    tokio::time::sleep(config.response_wait_time).await;
                    let _ = send_message_tx.send(PacketLossEvent::AllMessagesSent).await;
                    break;
                }

                // Sleep the designated time between batch
                tokio::time::sleep(config.batch_wait_time).await;

                // Note, shutdown may be delayed by the configured sleeps
                if shutdown.is_cancelled() {
                    tracing::debug!("Shutdown requested");
                    break;
                }
            }
        });
    }

    /// A GET/POST to an endpoint which sends/receives a large number of bytes
    /// as quickly as possible. The intent of these connections is to saturate
    /// a single connection's flow.
    #[tracing::instrument(skip_all)]
    fn new_load_generating_connection(
        &self,
        event_tx: mpsc::Sender<PacketLossEvent>,
        direction: Direction,
        network: Arc<dyn Network>,
        time: Arc<dyn Time>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<()> {
        let oneshot_res = self.load_generator.new_loaded_connection(
            direction,
            ConnectionType::H2,
            network,
            time,
            shutdown,
        )?;

        tokio::spawn(
            async move {
                let _ = match oneshot_res.await {
                    Ok(conn) => event_tx.send(PacketLossEvent::NewLoadedConnection(conn)),
                    Err(err) => event_tx.send(PacketLossEvent::Error(err)),
                }
                .await;
            }
            .in_current_span(),
        );

        Ok(())
    }
}

/// The result describes the number of UDP packets received and the loss ratio.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct PacketLossResult {
    /// Number of messages received during test
    pub num_messages: usize,
    /// Calculated ratio based on expected returned messages and actual
    pub loss_ratio: f64,
}

impl Display for PacketLossResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "messages: {} loss ratio: {:.2}",
            self.num_messages, self.loss_ratio,
        )
    }
}

pub enum PacketLossEvent {
    AllMessagesSent,
    NewLoadedConnection(LoadedConnection),
    Error(anyhow::Error),
}

/// A [`TurnServerCreds`] stores the fetched credentials required to communicat with the TURN server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TurnServerCreds {
    pub username: String,
    pub credential: String,
}

#[cfg(test)]
mod tests {
    use crate::{
        webrtc_data_channel::tests::TestTurnServer, PacketLoss, PacketLossConfig, PacketLossResult,
    };
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    async fn run_test(
        num_packets: usize,
        batch_size: usize,
        batch_wait_time: u64,
        response_wait_time: u64,
    ) -> anyhow::Result<PacketLossResult> {
        let shutdown = CancellationToken::new();
        let server = TestTurnServer::start_turn_server().await?;

        let config = PacketLossConfig {
            turn_server_uri: format!("turn:127.0.0.1:{}?transport=udp", server.server_port),
            turn_cred_request_url: "https://127.0.0.1/creds".parse().unwrap(),
            num_packets,
            batch_size,
            batch_wait_time: Duration::from_millis(batch_wait_time),
            response_wait_time: Duration::from_millis(response_wait_time),
            download_url: "https://h3.speed.cloudflare.com/__down?bytes=10000000000"
                .parse()
                .unwrap(),
            upload_url: "https://h3.speed.cloudflare.com/__up"
                .parse()
                .unwrap(),
        };
        let packet_loss = PacketLoss::new_with_config(config)?;
        let packet_loss_result = packet_loss
            .run_test(server.get_test_creds(), shutdown)
            .await;
        server.close().await?;
        packet_loss_result
    }

    #[tokio::test]
    async fn test_with_server() -> anyhow::Result<()> {
        // Basic packet loss test against the test server
        let packet_loss_result = run_test(1000, 10, 10, 100).await?;
        assert_eq!(
            PacketLossResult {
                num_messages: 1000,
                loss_ratio: 0.0,
            },
            packet_loss_result
        );
        println!("Results: {:?}", packet_loss_result);

        Ok(())
    }

    #[tokio::test]
    async fn test_no_batch_size() -> anyhow::Result<()> {
        // Test with zero batch size which results in one big batch
        let packet_loss_result = run_test(1000, 0, 10, 100).await?;
        assert_eq!(
            PacketLossResult {
                num_messages: 1000,
                loss_ratio: 0.0,
            },
            packet_loss_result
        );
        println!("Results: {:?}", packet_loss_result);

        Ok(())
    }

    #[tokio::test]
    async fn test_too_large_batch_size() -> anyhow::Result<()> {
        // Test batch size larger than total size
        let packet_loss_result = run_test(100, 1000, 10, 100).await?;
        assert_eq!(
            PacketLossResult {
                num_messages: 100,
                loss_ratio: 0.0,
            },
            packet_loss_result
        );
        println!("Results: {:?}", packet_loss_result);

        Ok(())
    }

    #[tokio::test]
    async fn test_equal_batch_size() -> anyhow::Result<()> {
        // Test one big batch
        let packet_loss_result = run_test(100, 100, 10, 100).await?;
        assert_eq!(
            PacketLossResult {
                num_messages: 100,
                loss_ratio: 0.0,
            },
            packet_loss_result
        );
        println!("Results: {:?}", packet_loss_result);

        Ok(())
    }

    #[tokio::test]
    async fn test_zero_batch_wait() -> anyhow::Result<()> {
        // Test without any waits between batches
        let packet_loss_result = run_test(100, 10, 0, 100).await?;
        assert_eq!(
            PacketLossResult {
                num_messages: 100,
                loss_ratio: 0.0,
            },
            packet_loss_result
        );
        println!("Results: {:?}", packet_loss_result);

        Ok(())
    }

    #[tokio::test]
    async fn test_cancel() -> anyhow::Result<()> {
        let shutdown = CancellationToken::new();
        let server = TestTurnServer::start_turn_server().await?;

        // Configure a large enough batch with enough batch wait to allow the test runs a bit longer
        let config = PacketLossConfig {
            turn_server_uri: format!("turn:127.0.0.1:{}?transport=udp", server.server_port),
            turn_cred_request_url: "https://127.0.0.1/creds".parse().unwrap(),
            num_packets: 5000,
            batch_size: 10,
            batch_wait_time: Duration::from_millis(25),
            response_wait_time: Duration::from_millis(100),
            download_url: "https://h3.speed.cloudflare.com/__down?bytes=10000000000"
                .parse()
                .unwrap(),
            upload_url: "https://h3.speed.cloudflare.com/__up"
                .parse()
                .unwrap(),
        };
        let packet_loss = PacketLoss::new_with_config(config)?;

        // Sleep before invoking cancel/shutdown
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5000)).await;
            shutdown_clone.cancel();
        });

        // Start the loss test
        let packet_loss_result = packet_loss
            .run_test(server.get_test_creds(), shutdown)
            .await;
        server.close().await?;
        let packet_loss_result = packet_loss_result?;
        assert_ne!(
            PacketLossResult {
                num_messages: 5000,
                loss_ratio: 0.0,
            },
            packet_loss_result
        );
        println!("Results: {:?}", packet_loss_result);

        Ok(())
    }
}
