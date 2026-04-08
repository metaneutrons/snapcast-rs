#![warn(unsafe_code)]
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
//! let (mut server, mut events) = SnapServer::new(config);
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

pub mod control;
pub mod encoder;
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
}

impl SnapServer {
    /// Create a new server. Returns the server and a receiver for events.
    pub fn new(config: ServerConfig) -> (Self, mpsc::Receiver<ServerEvent>) {
        let (event_tx, event_rx) = mpsc::channel(256);
        let (command_tx, command_rx) = mpsc::channel(64);
        let server = Self {
            config,
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
        };
        (server, event_rx)
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

        tracing::info!(
            stream_port = self.config.stream_port,
            http_port = self.config.http_port,
            "Snapserver starting"
        );

        // Parse default sample format
        let default_format: snapcast_proto::SampleFormat = self
            .config
            .sample_format
            .parse()
            .unwrap_or_else(|_| snapcast_proto::SampleFormat::new(48000, 16, 2));

        // Start stream manager with configured sources
        let mut manager = stream::manager::StreamManager::new();
        for source in &self.config.sources {
            if let Err(e) = manager.add_stream(source, default_format, &self.config.codec, "") {
                tracing::error!(source, error = %e, "Failed to add stream");
            }
        }

        // Get codec header from first stream for client handshake
        let first_stream = manager.stream_ids().into_iter().next().unwrap_or_default();
        let (codec, header, _format) =
            manager
                .header(&first_stream)
                .unwrap_or(("pcm", &[], default_format));
        let codec = codec.to_string();
        let header = header.to_vec();

        // Start session server
        let session_srv =
            session::SessionServer::new(self.config.stream_port, self.config.buffer_ms as i32);
        let chunk_sender = manager.chunk_sender();
        let session_event_tx = event_tx.clone();

        let session_handle = tokio::spawn(async move {
            if let Err(e) = session_srv
                .run(chunk_sender, codec, header, session_event_tx)
                .await
            {
                tracing::error!(error = %e, "Session server error");
            }
        });

        // Start control server (JSON-RPC over TCP)
        let shared_state = Arc::new(tokio::sync::Mutex::new(state::ServerState::default()));
        let (notify_tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(256);
        let control_state = Arc::clone(&shared_state);
        let control_event_tx = event_tx.clone();
        let control_notify_tx = notify_tx.clone();
        let control_port = self.config.control_port;

        let control_handle = tokio::spawn(async move {
            if let Err(e) = control::run_tcp(
                control_port,
                control_state,
                control_event_tx,
                control_notify_tx,
            )
            .await
            {
                tracing::error!(error = %e, "Control server error");
            }
        });

        // Main loop: handle commands + forward extension JSON-RPC responses
        loop {
            match command_rx.recv().await {
                Some(ServerCommand::Stop) | None => {
                    tracing::info!("Server stopping");
                    session_handle.abort();
                    control_handle.abort();
                    return Ok(());
                }
                Some(ServerCommand::SendJsonRpc { message, .. }) => {
                    // Broadcast JSON-RPC response/notification to control clients
                    let _ = notify_tx.send(message);
                }
            }
        }
    }
}
