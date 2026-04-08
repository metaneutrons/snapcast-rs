#![deny(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast client library — embeddable synchronized multiroom audio.
//!
//! # Example
//! ```no_run
//! use snapcast_client::{SnapClient, ClientConfig, ClientEvent, ClientCommand};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = ClientConfig::default();
//! let (mut client, mut events, mut audio_rx) = SnapClient::new(config);
//! let cmd = client.command_sender();
//!
//! // React to events in a separate task
//! tokio::spawn(async move {
//!     while let Some(event) = events.recv().await {
//!         match event {
//!             ClientEvent::VolumeChanged { volume, muted } => {
//!                 println!("Volume: {volume}, muted: {muted}");
//!             }
//!             _ => {}
//!         }
//!     }
//! });
//!
//! // Stop on Ctrl-C
//! let stop = cmd.clone();
//! tokio::spawn(async move {
//!     tokio::signal::ctrl_c().await.ok();
//!     stop.send(ClientCommand::Stop).await.ok();
//! });
//!
//! client.run().await?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod connection;
pub mod controller;
pub mod decoder;
pub mod double_buffer;
pub mod player;
pub mod stream;
pub mod time_provider;

#[cfg(feature = "mdns")]
pub mod discovery;

#[cfg(feature = "resampler")]
pub mod resampler;

use tokio::sync::mpsc;

pub use config::{ClientSettings, PlayerSettings, ServerSettings};
pub use snapcast_proto::SampleFormat;

/// Interleaved f32 audio frame produced by the client library.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Interleaved f32 samples (channel-interleaved).
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
    /// Server timestamp in microseconds (for sync).
    pub timestamp_usec: i64,
}

/// Events emitted by the client to the consumer.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Connected to server.
    Connected {
        /// Server hostname or IP.
        host: String,
        /// Server port.
        port: u16,
    },
    /// Disconnected from server.
    Disconnected {
        /// Reason for disconnection.
        reason: String,
    },
    /// Audio stream started with the given format.
    StreamStarted {
        /// Codec name (e.g. "flac", "opus").
        codec: String,
        /// PCM sample format.
        format: SampleFormat,
    },
    /// Server settings received or updated.
    ServerSettings {
        /// Playout buffer in milliseconds.
        buffer_ms: i32,
        /// Additional latency in milliseconds.
        latency: i32,
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Volume changed (from server or local).
    VolumeChanged {
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Time sync completed.
    TimeSyncComplete {
        /// Clock difference to server in milliseconds.
        diff_ms: f64,
    },
    /// Raw JSON-RPC message from server — extension point.
    JsonRpc(serde_json::Value),
}

/// Commands the consumer sends to the client.
#[derive(Debug, Clone)]
pub enum ClientCommand {
    /// Set volume (0–100) and mute state.
    SetVolume {
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Send arbitrary JSON-RPC to the server — extension point.
    SendJsonRpc(serde_json::Value),
    /// Stop the client gracefully.
    Stop,
}

/// Configuration for the embeddable client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Server connection settings.
    pub server: ServerSettings,
    /// Audio player settings.
    pub player: PlayerSettings,
    /// Instance id (for multiple clients on one host).
    pub instance: u32,
    /// Unique host identifier (default: MAC address).
    pub host_id: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server: ServerSettings::default(),
            player: PlayerSettings::default(),
            instance: 1,
            host_id: String::new(),
        }
    }
}

impl From<ClientSettings> for ClientConfig {
    fn from(s: ClientSettings) -> Self {
        Self {
            server: s.server,
            player: s.player,
            instance: s.instance,
            host_id: s.host_id,
        }
    }
}

/// The embeddable Snapcast client.
pub struct SnapClient {
    config: ClientConfig,
    event_tx: mpsc::Sender<ClientEvent>,
    command_tx: mpsc::Sender<ClientCommand>,
    command_rx: Option<mpsc::Receiver<ClientCommand>>,
    audio_tx: Option<mpsc::Sender<AudioFrame>>,
}

impl SnapClient {
    /// Create a new client. Returns the client, event receiver, and audio output receiver.
    pub fn new(
        config: ClientConfig,
    ) -> (
        Self,
        mpsc::Receiver<ClientEvent>,
        mpsc::Receiver<AudioFrame>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(256);
        let (command_tx, command_rx) = mpsc::channel(64);
        let (audio_tx, audio_rx) = mpsc::channel(256);
        let client = Self {
            config,
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            audio_tx: Some(audio_tx),
        };
        (client, event_rx, audio_rx)
    }

    /// Get a cloneable command sender.
    pub fn command_sender(&self) -> mpsc::Sender<ClientCommand> {
        self.command_tx.clone()
    }

    /// Run the client. Blocks until stopped or a fatal error occurs.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let command_rx = self
            .command_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("run() already called"))?;

        let settings = ClientSettings {
            instance: self.config.instance,
            host_id: self.config.host_id.clone(),
            server: self.config.server.clone(),
            player: self.config.player.clone(),
            logging: config::LoggingSettings::default(),
            #[cfg(unix)]
            daemon: None,
        };

        let audio_tx = self
            .audio_tx
            .take()
            .ok_or_else(|| anyhow::anyhow!("run() already called"))?;

        let mut ctrl =
            controller::Controller::new(settings, self.event_tx.clone(), command_rx, audio_tx);
        ctrl.run().await
    }
}
