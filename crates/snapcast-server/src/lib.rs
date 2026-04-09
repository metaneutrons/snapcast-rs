#![deny(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast server library — embeddable synchronized multiroom audio server.
//!
//! # Architecture
//!
//! The server is built around a channel-based API matching [`snapcast_client`]:
//!
//! - [`SnapServer`] is the main entry point
//! - [`ServerEvent`] flows from server → consumer (client connected, stream status, JSON-RPC)
//! - [`ServerCommand`] flows from consumer → server (send JSON-RPC, stop)
//!
//! # JSON-RPC Extension Point
//!
//! Unrecognized JSON-RPC methods are forwarded as [`ServerEvent::JsonRpc`].
//! The embedding application handles them and responds via [`ServerCommand::SendJsonRpc`].
//! This enables custom features (e.g. EQ control) without modifying the library.
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
//! // Handle events (including custom JSON-RPC)
//! tokio::spawn(async move {
//!     while let Some(event) = events.recv().await {
//!         match event {
//!             ServerEvent::JsonRpc { client_id, request, response_tx } => {
//!                 if request["method"] == "Client.SetEq" {
//!                     let response = serde_json::json!({
//!                         "jsonrpc": "2.0",
//!                         "id": request["id"],
//!                         "result": { "ok": true }
//!                     });
//!                     if let Some(tx) = response_tx {
//!                         tx.send(response).ok();
//!                     }
//!                 }
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

pub mod encoder;
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
    /// A client's volume changed (from JSON-RPC or typed API).
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
    /// Custom JSON-RPC request from a registered method or notification.
    JsonRpc {
        /// Control client that sent the request.
        client_id: String,
        /// The full JSON-RPC request object.
        request: serde_json::Value,
        /// Response channel (Some for methods, None for notifications).
        response_tx: Option<tokio::sync::oneshot::Sender<serde_json::Value>>,
    },
}

/// Commands the consumer sends to the server.
#[derive(Debug)]
pub enum ServerCommand {
    /// Send a JSON-RPC response or notification to control client(s).
    SendJsonRpc {
        /// Target control client, or `None` for broadcast.
        client_id: Option<String>,
        /// The JSON-RPC message to send.
        message: serde_json::Value,
    },
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
    /// Stop the server gracefully.
    Stop,
}

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// TCP port for binary protocol (client connections). Default: 1704.
    pub stream_port: u16,
    /// TCP port for JSON-RPC control. Default: 1705.
    pub control_port: u16,
    /// HTTP port for JSON-RPC + Snapweb. Default: 1780.
    pub http_port: u16,
    /// Path to Snapweb static files (None = disabled).
    pub doc_root: Option<String>,
    /// Audio buffer size in milliseconds. Default: 1000.
    pub buffer_ms: u32,
    /// Default codec: "f32lz4", "pcm", "opus", "ogg". Default: "f32lz4".
    pub codec: String,
    /// Default sample format. Default: 48000:16:2.
    pub sample_format: String,
    /// Stream source URIs (from config file [stream] source= lines).
    pub sources: Vec<String>,
    /// Path to server state file for persistence.
    pub state_file: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            stream_port: 1704,
            control_port: 1705,
            http_port: 1780,
            doc_root: None,
            buffer_ms: 1000,
            codec: "f32lz4".into(),
            sample_format: "48000:16:2".into(),
            sources: vec!["pipe:///tmp/snapfifo?name=default".into()],
            state_file: Some("/var/lib/snapserver/server.json".into()),
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
        let (event_tx, event_rx) = mpsc::channel(256);
        let (command_tx, command_rx) = mpsc::channel(64);
        let (audio_tx, audio_rx) = mpsc::channel(256);
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

        let manager = self.manager.take().unwrap_or_default();

        let default_format = snapcast_proto::SampleFormat::new(48000, 16, 2);
        let first_stream = manager.stream_ids().into_iter().next().unwrap_or_default();
        let (codec, header) = if let Some((c, h, _)) = manager.header(&first_stream) {
            (c.to_string(), h.to_vec())
        } else {
            let enc = encoder::create(&self.config.codec, default_format, "")?;
            (self.config.codec.clone(), enc.header().to_vec())
        };

        let chunk_sender = manager.chunk_sender();
        let audio_chunk_sender = chunk_sender.clone();

        // Start session server
        let session_srv = Arc::new(session::SessionServer::new(
            self.config.stream_port,
            self.config.buffer_ms as i32,
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
                        Some(ServerCommand::SendJsonRpc { .. }) => {
                            // JSON-RPC is handled by the binary, not the library
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
                                g.name = name;
                            }
                        }
                        Some(ServerCommand::SetGroupClients { group_id, clients }) => {
                            let mut s = shared_state.lock().await;
                            for cid in &clients {
                                s.remove_client_from_groups(cid);
                            }
                            if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                                g.clients = clients;
                            }
                        }
                        Some(ServerCommand::DeleteClient { client_id }) => {
                            let mut s = shared_state.lock().await;
                            s.remove_client_from_groups(&client_id);
                            s.clients.remove(&client_id);
                        }
                        Some(ServerCommand::GetStatus { response_tx }) => {
                            let s = shared_state.lock().await;
                            let _ = response_tx.send(s.to_status_json());
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
                                let i = (s * i16::MAX as f32) as i16;
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
