#![deny(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast server library — embeddable synchronized multiroom audio server.
//!
//! See also: [`snapcast-client`](https://docs.rs/snapcast-client) for the client library.
//! # Architecture
//!
//! The server is built around a channel-based API matching `snapcast-client`:
//!
//! - [`SnapServer`] is the main entry point
//! - [`ServerEvent`] flows from server → consumer (client connected, stream status, custom messages)
//! - [`ServerCommand`] flows from consumer → server (typed mutations, custom messages, stop)
//!
//! # Example
//!
//! ```no_run
//! use snapcast_server::{SnapServer, ServerConfig, ServerEvent, ServerCommand};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = ServerConfig::default();
//! let (mut server, mut events, _audio_tx) = SnapServer::new(config);
//! let cmd = server.command_sender();
//!
//! tokio::spawn(async move {
//!     while let Some(event) = events.recv().await {
//!         match event {
//!             ServerEvent::ClientConnected { id, name } => {
//!                 tracing::info!(id, name, "Client connected");
//!             }
//!             _ => {}
//!         }
//!     }
//! });
//!
//! server.run().await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use tokio::sync::mpsc;

// Re-export proto types that embedders need
#[cfg(feature = "custom-protocol")]
pub use snapcast_proto::CustomMessage;
pub use snapcast_proto::SampleFormat;
pub use snapcast_proto::{DEFAULT_SAMPLE_FORMAT, DEFAULT_STREAM_PORT};

const EVENT_CHANNEL_SIZE: usize = 256;
const COMMAND_CHANNEL_SIZE: usize = 64;
const AUDIO_CHANNEL_SIZE: usize = 256;

/// Interleaved f32 audio frame for server input.
#[derive(Debug)]
pub struct AudioFrame {
    /// Interleaved f32 samples.
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
    /// Timestamp in microseconds.
    pub timestamp_usec: i64,
}

pub mod auth;
#[cfg(feature = "encryption")]
pub mod crypto;
pub mod encoder;
#[cfg(feature = "mdns")]
pub mod mdns;
pub mod session;
pub mod state;
pub mod stream;
pub mod time;

/// Settings update pushed to a streaming client via binary protocol.
#[derive(Debug, Clone)]
pub struct ClientSettingsUpdate {
    /// Target client ID.
    pub client_id: String,
    /// Buffer size in ms.
    pub buffer_ms: i32,
    /// Latency offset in ms.
    pub latency: i32,
    /// Volume (0–100).
    pub volume: u16,
    /// Mute state.
    pub muted: bool,
}

/// Events emitted by the server to the consumer.
#[derive(Debug)]
pub enum ServerEvent {
    /// A client connected via the binary protocol.
    ClientConnected {
        /// Unique client identifier.
        id: String,
        /// Client hostname.
        name: String,
    },
    /// A client disconnected.
    ClientDisconnected {
        /// Unique client identifier.
        id: String,
    },
    /// A client's volume changed.
    ClientVolumeChanged {
        /// Client ID.
        client_id: String,
        /// New volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// A client's latency changed.
    ClientLatencyChanged {
        /// Client ID.
        client_id: String,
        /// New latency in ms.
        latency: i32,
    },
    /// A client's name changed.
    ClientNameChanged {
        /// Client ID.
        client_id: String,
        /// New name.
        name: String,
    },
    /// A group's stream assignment changed.
    GroupStreamChanged {
        /// Group ID.
        group_id: String,
        /// New stream ID.
        stream_id: String,
    },
    /// A group's mute state changed.
    GroupMuteChanged {
        /// Group ID.
        group_id: String,
        /// Mute state.
        muted: bool,
    },
    /// A stream's status changed (playing, idle, unknown).
    StreamStatus {
        /// Stream identifier.
        stream_id: String,
        /// New status.
        status: String,
    },
    /// A group's name changed.
    GroupNameChanged {
        /// Group ID.
        group_id: String,
        /// New name.
        name: String,
    },
    /// Server state changed — groups were reorganized (created, deleted, merged).
    ///
    /// Emitted after structural changes like `SetGroupClients` or `DeleteClient`
    /// when the group topology changes. Mirrors `Server.OnUpdate` in the C++ snapserver.
    /// The consumer should re-read server status via `GetStatus`.
    ServerUpdated,
    /// Custom binary protocol message from a streaming client.
    #[cfg(feature = "custom-protocol")]
    CustomMessage {
        /// Client ID.
        client_id: String,
        /// The custom message.
        message: snapcast_proto::CustomMessage,
    },
}

/// Commands the consumer sends to the server.
#[derive(Debug)]
pub enum ServerCommand {
    /// Set a client's volume.
    SetClientVolume {
        /// Client ID.
        client_id: String,
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Set a client's latency offset.
    SetClientLatency {
        /// Client ID.
        client_id: String,
        /// Latency in milliseconds.
        latency: i32,
    },
    /// Set a client's display name.
    SetClientName {
        /// Client ID.
        client_id: String,
        /// New name.
        name: String,
    },
    /// Assign a stream to a group.
    SetGroupStream {
        /// Group ID.
        group_id: String,
        /// Stream ID.
        stream_id: String,
    },
    /// Mute/unmute a group.
    SetGroupMute {
        /// Group ID.
        group_id: String,
        /// Mute state.
        muted: bool,
    },
    /// Set a group's display name.
    SetGroupName {
        /// Group ID.
        group_id: String,
        /// New name.
        name: String,
    },
    /// Move clients to a group.
    SetGroupClients {
        /// Group ID.
        group_id: String,
        /// Client IDs.
        clients: Vec<String>,
    },
    /// Delete a client from the server.
    DeleteClient {
        /// Client ID.
        client_id: String,
    },
    /// Get full server status.
    GetStatus {
        /// Response channel.
        response_tx: tokio::sync::oneshot::Sender<serde_json::Value>,
    },
    /// Send a custom binary protocol message to a streaming client.
    #[cfg(feature = "custom-protocol")]
    SendToClient {
        /// Target client ID.
        client_id: String,
        /// The custom message.
        message: snapcast_proto::CustomMessage,
    },
    /// Stop the server gracefully.
    Stop,
}

/// Default codec based on compiled features.
fn default_codec() -> &'static str {
    #[cfg(feature = "flac")]
    return "flac";
    #[cfg(all(feature = "f32lz4", not(feature = "flac")))]
    return "f32lz4";
    #[cfg(not(any(feature = "flac", feature = "f32lz4")))]
    return "pcm";
}

/// Server configuration for the embeddable library.
pub struct ServerConfig {
    /// TCP port for binary protocol (client connections). Default: 1704.
    pub stream_port: u16,
    /// Audio buffer size in milliseconds. Default: 1000.
    pub buffer_ms: u32,
    /// Default codec: "f32lz4", "pcm", "opus", "ogg". Default: "f32lz4".
    pub codec: String,
    /// Default sample format. Default: 48000:16:2.
    pub sample_format: String,
    /// mDNS service type. Default: "_snapcast._tcp.local.".
    pub mdns_service_type: String,
    /// Auth validator for streaming clients. `None` = no auth required.
    pub auth: Option<std::sync::Arc<dyn auth::AuthValidator>>,
    /// Pre-shared key for f32lz4 encryption. `None` = no encryption.
    #[cfg(feature = "encryption")]
    pub encryption_psk: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            stream_port: snapcast_proto::DEFAULT_STREAM_PORT,
            buffer_ms: 1000,
            codec: default_codec().into(),
            sample_format: "48000:16:2".into(),
            mdns_service_type: "_snapcast._tcp.local.".into(),
            auth: None,
            #[cfg(feature = "encryption")]
            encryption_psk: None,
        }
    }
}

/// The embeddable Snapcast server.
pub struct SnapServer {
    config: ServerConfig,
    event_tx: mpsc::Sender<ServerEvent>,
    command_tx: mpsc::Sender<ServerCommand>,
    command_rx: Option<mpsc::Receiver<ServerCommand>>,
    audio_rx: Option<mpsc::Receiver<crate::AudioFrame>>,
    manager: Option<stream::manager::StreamManager>,
}

impl SnapServer {
    /// Create a new server. Returns the server, event receiver, and audio input sender.
    ///
    /// The `audio_tx` sender allows the embedding app to push PCM audio directly
    /// into the server as an alternative to pipe/file/process stream readers.
    pub fn new(
        config: ServerConfig,
    ) -> (
        Self,
        mpsc::Receiver<ServerEvent>,
        mpsc::Sender<crate::AudioFrame>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_SIZE);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_SIZE);
        let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_SIZE);
        let server = Self {
            config,
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            audio_rx: Some(audio_rx),
            manager: None,
        };
        (server, event_rx, audio_tx)
    }

    /// Set the stream manager (configured by the binary with stream readers).
    pub fn set_manager(&mut self, manager: stream::manager::StreamManager) {
        self.manager = Some(manager);
    }

    /// Get a cloneable command sender.
    pub fn command_sender(&self) -> mpsc::Sender<ServerCommand> {
        self.command_tx.clone()
    }

    /// Access the server configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Run the server. Blocks until stopped or a fatal error occurs.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let mut command_rx = self
            .command_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("run() already called"))?;

        let mut audio_rx = self
            .audio_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("run() already called"))?;

        let event_tx = self.event_tx.clone();

        tracing::info!(stream_port = self.config.stream_port, "Snapserver starting");

        // Advertise via mDNS (protocol-level discovery)
        #[cfg(feature = "mdns")]
        let _mdns =
            mdns::MdnsAdvertiser::new(self.config.stream_port, &self.config.mdns_service_type)
                .map_err(|e| tracing::warn!(error = %e, "mDNS advertisement failed"))
                .ok();

        let manager = self.manager.take().unwrap_or_default();

        let default_format = snapcast_proto::DEFAULT_SAMPLE_FORMAT;
        let first_stream = manager.stream_ids().into_iter().next().unwrap_or_default();
        let (codec, header) = if let Some((c, h, _)) = manager.header(&first_stream) {
            (c.to_string(), h.to_vec())
        } else {
            let enc_config = encoder::EncoderConfig {
                codec: self.config.codec.clone(),
                format: default_format,
                options: String::new(),
                #[cfg(feature = "encryption")]
                encryption_psk: self.config.encryption_psk.clone(),
            };
            let enc = encoder::create(&enc_config)?;
            (self.config.codec.clone(), enc.header().to_vec())
        };

        let chunk_sender = manager.chunk_sender();
        let audio_chunk_sender = chunk_sender.clone();

        // Start session server
        let session_srv = Arc::new(session::SessionServer::new(
            self.config.stream_port,
            self.config.buffer_ms as i32,
            self.config.auth.clone(),
        ));
        let session_for_run = Arc::clone(&session_srv);
        let session_event_tx = event_tx.clone();
        let session_handle = tokio::spawn(async move {
            if let Err(e) = session_for_run
                .run(chunk_sender, codec, header, session_event_tx)
                .await
            {
                tracing::error!(error = %e, "Session server error");
            }
        });

        // Shared state for command handlers
        let shared_state = Arc::new(tokio::sync::Mutex::new(state::ServerState::default()));

        // Main loop
        loop {
            tokio::select! {
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(ServerCommand::Stop) | None => {
                            tracing::info!("Server stopped");
                            session_handle.abort();
                            return Ok(());
                        }
                        Some(ServerCommand::SetClientVolume { client_id, volume, muted }) => {
                            let mut s = shared_state.lock().await;
                            if let Some(c) = s.clients.get_mut(&client_id) {
                                c.config.volume.percent = volume;
                                c.config.volume.muted = muted;
                            }
                            session_srv.push_settings(ClientSettingsUpdate {
                                client_id: client_id.clone(),
                                buffer_ms: self.config.buffer_ms as i32,
                                latency: 0, volume, muted,
                            }).await;
                            let _ = event_tx.try_send(ServerEvent::ClientVolumeChanged { client_id, volume, muted });
                        }
                        Some(ServerCommand::SetClientLatency { client_id, latency }) => {
                            let mut s = shared_state.lock().await;
                            if let Some(c) = s.clients.get_mut(&client_id) {
                                c.config.latency = latency;
                                session_srv.push_settings(ClientSettingsUpdate {
                                    client_id: client_id.clone(),
                                    buffer_ms: self.config.buffer_ms as i32,
                                    latency,
                                    volume: c.config.volume.percent,
                                    muted: c.config.volume.muted,
                                }).await;
                            }
                            let _ = event_tx.try_send(ServerEvent::ClientLatencyChanged { client_id, latency });
                        }
                        Some(ServerCommand::SetClientName { client_id, name }) => {
                            let mut s = shared_state.lock().await;
                            if let Some(c) = s.clients.get_mut(&client_id) {
                                c.config.name = name.clone();
                            }
                            let _ = event_tx.try_send(ServerEvent::ClientNameChanged { client_id, name });
                        }
                        Some(ServerCommand::SetGroupStream { group_id, stream_id }) => {
                            shared_state.lock().await.set_group_stream(&group_id, &stream_id);
                            let _ = event_tx.try_send(ServerEvent::GroupStreamChanged { group_id, stream_id });
                        }
                        Some(ServerCommand::SetGroupMute { group_id, muted }) => {
                            let mut s = shared_state.lock().await;
                            if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                                g.muted = muted;
                            }
                            let _ = event_tx.try_send(ServerEvent::GroupMuteChanged { group_id, muted });
                        }
                        Some(ServerCommand::SetGroupName { group_id, name }) => {
                            let mut s = shared_state.lock().await;
                            if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                                g.name = name.clone();
                            }
                            let _ = event_tx.try_send(ServerEvent::GroupNameChanged { group_id, name });
                        }
                        Some(ServerCommand::SetGroupClients { group_id, clients }) => {
                            let mut s = shared_state.lock().await;
                            for cid in &clients {
                                s.remove_client_from_groups(cid);
                            }
                            if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                                g.clients = clients;
                            }
                            // Structural change — mirrors Server.OnUpdate in C++ snapserver
                            let _ = event_tx.try_send(ServerEvent::ServerUpdated);
                        }
                        Some(ServerCommand::DeleteClient { client_id }) => {
                            let mut s = shared_state.lock().await;
                            s.remove_client_from_groups(&client_id);
                            s.clients.remove(&client_id);
                            drop(s);
                            let _ = event_tx.try_send(ServerEvent::ServerUpdated);
                        }
                        Some(ServerCommand::GetStatus { response_tx }) => {
                            let s = shared_state.lock().await;
                            let _ = response_tx.send(s.to_status_json());
                        }
                        #[cfg(feature = "custom-protocol")]
                        Some(ServerCommand::SendToClient { client_id, message }) => {
                            session_srv.send_custom(&client_id, message.type_id, message.payload).await;
                        }
                    }
                }
                frame = audio_rx.recv() => {
                    if let Some(frame) = frame {
                        let data = if self.config.codec == "f32lz4" {
                            frame.samples.iter().flat_map(|s| s.to_le_bytes()).collect()
                        } else {
                            let mut pcm = Vec::with_capacity(frame.samples.len() * 2);
                            for &s in &frame.samples {
                                let i = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                                pcm.extend_from_slice(&i.to_le_bytes());
                            }
                            pcm
                        };
                        let wire = stream::manager::WireChunkData {
                            stream_id: "external".into(),
                            timestamp_usec: frame.timestamp_usec,
                            data,
                        };
                        let _ = audio_chunk_sender.send(wire);
                    }
                }
            }
        }
    }
}
