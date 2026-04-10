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
//! let (mut server, mut events) = SnapServer::new(config);
//! let _audio_tx = server.add_stream("default");
//! let cmd = server.command_sender();
//!
//! tokio::spawn(async move {
//!     while let Some(event) = events.recv().await {
//!         match event {
//!             ServerEvent::ClientConnected { id, name, .. } => {
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

use tokio::sync::{broadcast, mpsc};

// Re-export proto types that embedders need
#[cfg(feature = "custom-protocol")]
pub use snapcast_proto::CustomMessage;
#[cfg(feature = "encryption")]
pub use snapcast_proto::DEFAULT_ENCRYPTION_PSK;
pub use snapcast_proto::SampleFormat;
pub use snapcast_proto::{DEFAULT_SAMPLE_FORMAT, DEFAULT_STREAM_PORT};

const EVENT_CHANNEL_SIZE: usize = 256;
const COMMAND_CHANNEL_SIZE: usize = 64;
const AUDIO_CHANNEL_SIZE: usize = 256;

/// Audio data pushed by the consumer — either f32 or raw PCM.
#[derive(Debug, Clone)]
pub enum AudioData {
    /// Interleaved f32 samples (from DSP, EQ, AirPlay receivers).
    /// Range: -1.0 to 1.0.
    F32(Vec<f32>),
    /// Raw interleaved PCM bytes at the stream's configured sample format
    /// (from pipe/file/process readers). Byte order: little-endian.
    Pcm(Vec<u8>),
}

/// A timestamped audio frame for server input.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Audio samples.
    pub data: AudioData,
    /// Timestamp in microseconds (server time).
    pub timestamp_usec: i64,
}

/// An encoded audio chunk ready to be sent to clients.
#[derive(Debug, Clone)]
pub struct WireChunkData {
    /// Stream ID this chunk belongs to.
    pub stream_id: String,
    /// Server timestamp in microseconds.
    pub timestamp_usec: i64,
    /// Encoded audio data.
    pub data: Vec<u8>,
}

pub mod auth;
#[cfg(feature = "encryption")]
pub mod crypto;
pub(crate) mod encoder;
#[cfg(feature = "mdns")]
pub mod mdns;
pub mod session;
pub mod state;
pub(crate) mod stream;
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
        /// Client MAC address.
        mac: String,
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

/// Per-stream configuration. If `None`, inherits from [`ServerConfig`].
#[derive(Debug, Clone, Default)]
pub struct StreamConfig {
    /// Codec override (e.g. "flac", "f32lz4", "opus", "ogg", "pcm").
    pub codec: Option<String>,
    /// Sample format override (e.g. "48000:16:2").
    pub sample_format: Option<String>,
}

/// The embeddable Snapcast server.
pub struct SnapServer {
    config: ServerConfig,
    event_tx: mpsc::Sender<ServerEvent>,
    command_tx: mpsc::Sender<ServerCommand>,
    command_rx: Option<mpsc::Receiver<ServerCommand>>,
    /// Named audio streams — each gets its own encoder at run().
    streams: Vec<(String, StreamConfig, mpsc::Receiver<AudioFrame>)>,
    /// Broadcast channel for encoded chunks → sessions.
    chunk_tx: broadcast::Sender<WireChunkData>,
}

/// Spawn a per-stream encode loop on a dedicated thread.
///
/// Receives `AudioFrame`, passes `AudioData` directly to the encoder,
/// and broadcasts encoded `WireChunkData` to sessions.
fn spawn_stream_encoder(
    stream_id: String,
    mut rx: mpsc::Receiver<AudioFrame>,
    mut enc: Box<dyn encoder::Encoder>,
    chunk_tx: broadcast::Sender<WireChunkData>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("encoder runtime");

        rt.block_on(async {
            while let Some(frame) = rx.recv().await {
                match enc.encode(&frame.data) {
                    Ok(encoded) if !encoded.data.is_empty() => {
                        let _ = chunk_tx.send(WireChunkData {
                            stream_id: stream_id.clone(),
                            timestamp_usec: frame.timestamp_usec,
                            data: encoded.data,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(stream = %stream_id, error = %e, "Encode failed");
                    }
                    _ => {} // encoder buffering
                }
            }
        });
    });
}

/// Convert f32 samples to PCM bytes at the given bit depth.
impl SnapServer {
    /// Create a new server. Returns the server and event receiver.
    pub fn new(config: ServerConfig) -> (Self, mpsc::Receiver<ServerEvent>) {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_SIZE);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_SIZE);
        let (chunk_tx, _) = broadcast::channel(256);
        let server = Self {
            config,
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            streams: Vec::new(),
            chunk_tx,
        };
        (server, event_rx)
    }

    /// Add a named audio stream. Returns a sender for pushing audio frames.
    ///
    /// Uses the server's default codec and sample format.
    pub fn add_stream(&mut self, name: &str) -> mpsc::Sender<AudioFrame> {
        self.add_stream_with_config(name, StreamConfig::default())
    }

    /// Add a named audio stream with per-stream codec/format overrides.
    pub fn add_stream_with_config(
        &mut self,
        name: &str,
        config: StreamConfig,
    ) -> mpsc::Sender<AudioFrame> {
        let (tx, rx) = mpsc::channel(AUDIO_CHANNEL_SIZE);
        self.streams.push((name.to_string(), config, rx));
        tx
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

        let event_tx = self.event_tx.clone();

        let sample_format: snapcast_proto::SampleFormat = self
            .config
            .sample_format
            .parse()
            .unwrap_or(snapcast_proto::DEFAULT_SAMPLE_FORMAT);

        anyhow::ensure!(
            !self.streams.is_empty(),
            "No streams configured — call add_stream() before run()"
        );

        tracing::info!(stream_port = self.config.stream_port, "Snapserver starting");

        // Advertise via mDNS (protocol-level discovery)
        #[cfg(feature = "mdns")]
        let _mdns =
            mdns::MdnsAdvertiser::new(self.config.stream_port, &self.config.mdns_service_type)
                .map_err(|e| tracing::warn!(error = %e, "mDNS advertisement failed"))
                .ok();

        // Create default encoder — used for codec header and first default stream
        let default_enc_config = encoder::EncoderConfig {
            codec: self.config.codec.clone(),
            format: sample_format,
            options: String::new(),
            #[cfg(feature = "encryption")]
            encryption_psk: self.config.encryption_psk.clone(),
        };
        let default_enc = encoder::create(&default_enc_config)?;
        let codec = default_enc.name().to_string();
        let codec_header = default_enc.header().to_vec();

        // Spawn per-stream encode loops — reuse default_enc for first default stream
        let chunk_tx = self.chunk_tx.clone();
        let streams = std::mem::take(&mut self.streams);
        let mut default_enc = Some(default_enc);
        for (name, stream_cfg, rx) in streams {
            let enc = if stream_cfg.codec.is_none() && stream_cfg.sample_format.is_none() {
                if let Some(enc) = default_enc.take() {
                    enc
                } else {
                    encoder::create(&default_enc_config)?
                }
            } else {
                let stream_codec = stream_cfg.codec.as_deref().unwrap_or(&self.config.codec);
                let stream_format: snapcast_proto::SampleFormat = stream_cfg
                    .sample_format
                    .as_deref()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(sample_format);
                encoder::create(&encoder::EncoderConfig {
                    codec: stream_codec.to_string(),
                    format: stream_format,
                    options: String::new(),
                    #[cfg(feature = "encryption")]
                    encryption_psk: self.config.encryption_psk.clone(),
                })?
            };
            tracing::info!(stream = %name, codec = enc.name(), %sample_format, "Stream registered");
            spawn_stream_encoder(name, rx, enc, chunk_tx.clone());
        }

        // Start session server
        let session_srv = Arc::new(session::SessionServer::new(
            self.config.stream_port,
            self.config.buffer_ms as i32,
            self.config.auth.clone(),
        ));
        let session_for_run = Arc::clone(&session_srv);
        let session_event_tx = event_tx.clone();
        let session_chunk_tx = self.chunk_tx.clone();
        let session_handle = tokio::spawn(async move {
            if let Err(e) = session_for_run
                .run(session_chunk_tx, codec, codec_header, session_event_tx)
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
            }
        }
    }
}
