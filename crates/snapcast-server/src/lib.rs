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

use tokio::sync::mpsc;

pub mod encoder;

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
        let _command_rx = self
            .command_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("run() already called"))?;

        let _event_tx = self.event_tx.clone();

        tracing::info!(
            stream_port = self.config.stream_port,
            http_port = self.config.http_port,
            "Snapserver starting"
        );

        // TODO: Phase 1+ implementation
        // - Start stream readers from config.sources
        // - Start stream server (binary protocol) on stream_port
        // - Start control server (JSON-RPC) on control_port + http_port
        // - Main select loop: stream chunks, client sessions, commands

        anyhow::bail!("server not yet implemented")
    }
}
