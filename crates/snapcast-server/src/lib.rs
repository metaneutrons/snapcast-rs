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
//!             ServerEvent::JsonRpc { client_id, request } => {
//!                 if request["method"] == "Client.SetEq" {
//!                     // Handle custom EQ method
//!                     let response = serde_json::json!({
//!                         "jsonrpc": "2.0",
//!                         "id": request["id"],
//!                         "result": { "ok": true }
//!                     });
//!                     cmd.send(ServerCommand::SendJsonRpc {
//!                         client_id: Some(client_id),
//!                         message: response,
//!                     }).await.ok();
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
#[derive(Debug, Clone)]
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
pub mod control;
pub mod encoder;
pub mod http;
pub mod jsonrpc;
pub mod session;
pub mod state;
pub mod stream;
pub mod time;

/// Events emitted by the server to the consumer.
#[derive(Debug, Clone)]
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
    /// A stream's status changed (playing, idle, unknown).
    StreamStatus {
        /// Stream identifier.
        stream_id: String,
        /// New status.
        status: String,
    },
    /// Unrecognized JSON-RPC request — extension point for custom methods.
    ///
    /// The embedding application should handle this and respond via
    /// [`ServerCommand::SendJsonRpc`].
    JsonRpc {
        /// Control client that sent the request.
        client_id: String,
        /// The full JSON-RPC request object.
        request: serde_json::Value,
    },
}

/// Commands the consumer sends to the server.
#[derive(Debug, Clone)]
pub enum ServerCommand {
    /// Send a JSON-RPC response or notification to control client(s).
    ///
    /// If `client_id` is `None`, broadcasts to all control clients.
    SendJsonRpc {
        /// Target control client, or `None` for broadcast.
        client_id: Option<String>,
        /// The JSON-RPC message to send.
        message: serde_json::Value,
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
    /// Default codec: "flac", "pcm", "opus", "ogg". Default: "flac".
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
            codec: "flac".into(),
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

        tracing::info!(
            stream_port = self.config.stream_port,
            http_port = self.config.http_port,
            "Snapserver starting"
        );

        // Advertise via mDNS

        // The binary must set up the StreamManager and pass it via set_manager()
        let manager = self.manager.take().unwrap_or_default();

        // Get codec header from first stream for client handshake
        let first_stream = manager.stream_ids().into_iter().next().unwrap_or_default();
        let default_format = snapcast_proto::SampleFormat::new(48000, 16, 2);
        let (codec, header, _format) =
            manager
                .header(&first_stream)
                .unwrap_or(("pcm", &[], default_format));
        let codec = codec.to_string();
        let header = header.to_vec();

        let chunk_sender = manager.chunk_sender();
        let audio_chunk_sender = chunk_sender.clone();
        let session_event_tx = event_tx.clone();

        // Start session server (shared for settings push)
        let session_srv = Arc::new(session::SessionServer::new(
            self.config.stream_port,
            self.config.buffer_ms as i32,
        ));
        let session_for_run = Arc::clone(&session_srv);
        let session_handle = tokio::spawn(async move {
            if let Err(e) = session_for_run
                .run(chunk_sender, codec, header, session_event_tx)
                .await
            {
                tracing::error!(error = %e, "Session server error");
            }
        });

        // Start control server (JSON-RPC over TCP)
        let shared_state = Arc::new(tokio::sync::Mutex::new(state::ServerState::default()));
        let (notify_tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(256);
        let (stream_ctrl_tx, mut stream_ctrl_rx) =
            tokio::sync::mpsc::channel::<jsonrpc::StreamControlMsg>(64);
        let (settings_push_tx, mut settings_push_rx) =
            tokio::sync::mpsc::channel::<jsonrpc::ClientSettingsUpdate>(64);
        let auth_cfg = Arc::new(auth::AuthConfig::default());
        let control_state = Arc::clone(&shared_state);
        let control_event_tx = event_tx.clone();
        let control_notify_tx = notify_tx.clone();
        let control_port = self.config.control_port;
        let control_auth = Arc::clone(&auth_cfg);
        let control_stream_tx = stream_ctrl_tx.clone();
        let control_settings_tx = settings_push_tx.clone();
        let control_buffer_ms = self.config.buffer_ms as i32;

        let control_handle = tokio::spawn(async move {
            if let Err(e) = control::run_tcp(control::ControlConfig {
                port: control_port,
                state: control_state,
                event_tx: control_event_tx,
                notify_tx: control_notify_tx,
                auth_config: control_auth,
                stream_control_tx: control_stream_tx,
                settings_tx: control_settings_tx,
                buffer_ms: control_buffer_ms,
            })
            .await
            {
                tracing::error!(error = %e, "Control server error");
            }
        });

        // Start HTTP/WebSocket server + Snapweb
        let http_state = Arc::clone(&shared_state);
        let http_event_tx = event_tx.clone();
        let http_notify_tx = notify_tx.clone();
        let http_auth = Arc::clone(&auth_cfg);
        let http_stream_tx = stream_ctrl_tx.clone();
        let http_settings_tx = settings_push_tx.clone();
        let http_port = self.config.http_port;
        let http_doc_root = self.config.doc_root.clone();
        let http_buffer_ms = self.config.buffer_ms as i32;

        let http_handle = tokio::spawn(async move {
            if let Err(e) = http::run_http(http::HttpConfig {
                port: http_port,
                doc_root: http_doc_root,
                state: http_state,
                event_tx: http_event_tx,
                notify_tx: http_notify_tx,
                auth_config: http_auth,
                stream_control_tx: http_stream_tx,
                settings_tx: http_settings_tx,
                buffer_ms: http_buffer_ms,
            })
            .await
            {
                tracing::error!(error = %e, "HTTP server error");
            }
        });

        // Main loop: handle commands, settings pushes, stream control
        loop {
            tokio::select! {
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(ServerCommand::Stop) | None => {
                            // Shut down mDNS first (daemon thread still alive)
                            tracing::info!("Server stopped");
                            session_handle.abort();
                            control_handle.abort();
                            http_handle.abort();
                            return Ok(());
                        }
                        Some(ServerCommand::SendJsonRpc { message, .. }) => {
                            let _ = notify_tx.send(message);
                        }
                    }
                }
                update = settings_push_rx.recv() => {
                    if let Some(update) = update {
                        session_srv.push_settings(update).await;
                    }
                }
                ctrl = stream_ctrl_rx.recv() => {
                    if let Some(ctrl) = ctrl {
                        tracing::debug!(
                            stream_id = %ctrl.stream_id,
                            command = %ctrl.command,
                            "Stream control (routing not yet implemented for process stdin)"
                        );
                    }
                }
                frame = audio_rx.recv() => {
                    if let Some(frame) = frame {
                        // Convert f32 to i16 PCM bytes
                        let mut pcm = Vec::with_capacity(frame.samples.len() * 2);
                        for &s in &frame.samples {
                            let i = (s * i16::MAX as f32) as i16;
                            pcm.extend_from_slice(&i.to_le_bytes());
                        }
                        // Broadcast as raw WireChunkData (bypasses encoder for now)
                        let wire = stream::manager::WireChunkData {
                            stream_id: "external".into(),
                            timestamp_usec: frame.timestamp_usec,
                            data: pcm,
                        };
                        let _ = audio_chunk_sender.send(wire);
                    }
                }
            }
        }
    }
}
