mod auth;
mod config;
mod control;
mod http;
mod jsonrpc;
mod stream;

use clap::Parser;
use snapcast_server::{ServerCommand, ServerEvent, SnapServer};

/// JSON-RPC event forwarded from control/HTTP handlers to the binary's event loop.
#[derive(Debug)]
pub(crate) enum ControlEvent {
    /// Unrecognized JSON-RPC method or registered notification.
    JsonRpc {
        /// Control client that sent the request.
        client_id: String,
        /// The full JSON-RPC request object.
        request: serde_json::Value,
        /// Response channel (`Some` for methods, `None` for notifications).
        _response_tx: Option<tokio::sync::oneshot::Sender<serde_json::Value>>,
    },
}

/// Snapcast server — synchronized multiroom audio server.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Config file path
    #[arg(short, long, default_value = "/etc/snapserver.conf")]
    config: String,

    /// TCP port for binary protocol (client connections)
    #[arg(long)]
    stream_port: Option<u16>,

    /// TCP port for JSON-RPC control
    #[arg(long)]
    control_port: Option<u16>,

    /// HTTP port for JSON-RPC + Snapweb
    #[arg(long)]
    http_port: Option<u16>,

    /// Path to Snapweb static files
    #[arg(long)]
    doc_root: Option<String>,

    /// Audio buffer size in milliseconds
    #[arg(long)]
    buffer: Option<u32>,

    /// Default codec: f32lz4, pcm, flac, opus, ogg
    #[arg(long)]
    codec: Option<String>,

    /// Default sample format
    #[arg(long)]
    sampleformat: Option<String>,

    /// Stream source URI (can be specified multiple times)
    #[arg(long = "source")]
    sources: Vec<String>,

    /// Log filter
    #[arg(long, default_value = "info")]
    logfilter: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(&cli.logfilter)
        .init();

    // Load config file, then merge CLI overrides
    let file_config = config::parse_config_file(&cli.config);
    let server_config = config::merge_cli(
        file_config,
        config::CliOverrides {
            stream_port: cli.stream_port,
            control_port: cli.control_port,
            http_port: cli.http_port,
            doc_root: cli.doc_root,
            buffer: cli.buffer,
            codec: cli.codec,
            sampleformat: cli.sampleformat,
            sources: cli.sources,
        },
    );

    let codec = server_config.server.codec.clone();
    let sample_format_str = server_config.server.sample_format.clone();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (mut server, mut events, _audio_tx) = SnapServer::new(server_config.server);

        // Ctrl-C handler — must be first so it works even if setup fails
        let cmd = server.command_sender();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received Ctrl-C, shutting down");
            cmd.send(ServerCommand::Stop).await.ok();
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                std::process::exit(0);
            });
        });

        // Set up stream manager with configured sources
        let default_format: snapcast_proto::SampleFormat = sample_format_str
            .parse()
            .unwrap_or(snapcast_proto::DEFAULT_SAMPLE_FORMAT);

        let mut manager = snapcast_server::stream::manager::StreamManager::new();
        for source in &server_config.sources {
            let parsed = match stream::uri::StreamUri::parse(source) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(source, error = %e, "Skipping malformed stream URI");
                    continue;
                }
            };
            let name = parsed.param("name").unwrap_or("default").to_string();
            let format = parsed
                .param("sampleformat")
                .and_then(|s| s.parse().ok())
                .unwrap_or(default_format);

            let (tx, rx) = tokio::sync::mpsc::channel(128);

            // Chunk size matches codec block size:
            // FLAC level 0-2: 1152 frames, level 3+: 4096 frames
            // Others: 960 frames (20ms at 48kHz)
            let chunk_frames = match codec.as_str() {
                "flac" => 1152,
                _ => (format.rate() as usize * 20) / 1000, // 20ms
            };

            // Start stream reader
            let reader_handle = match parsed.scheme.as_str() {
                "pipe" => stream::pipe::start(parsed, format, chunk_frames, tx),
                "file" => stream::file::start(parsed, format, chunk_frames, tx),
                "process" => stream::process::start(parsed, format, chunk_frames, tx),
                "tcp" => stream::tcp::start(parsed, format, chunk_frames, tx),
                "librespot" => {
                    let (meta_tx, _) = tokio::sync::mpsc::channel(32);
                    stream::librespot::start(parsed, format, tx, meta_tx)
                }
                "airplay" => {
                    let (meta_tx, _) = tokio::sync::mpsc::channel(32);
                    stream::airplay::start(parsed, format, tx, meta_tx)
                }
                other => {
                    tracing::error!(scheme = other, "Unsupported stream scheme");
                    continue;
                }
            };

            match reader_handle {
                Ok(_handle) => {
                    if let Err(e) = manager.add_stream_from_receiver(&name, format, &codec, "", rx)
                    {
                        tracing::error!(name, error = %e, "Failed to add stream");
                    }
                }
                Err(e) => tracing::error!(source, error = %e, "Failed to start stream reader"),
            }
        }

        server.set_manager(manager);

        // mDNS

        // JSON-RPC control servers
        let shared_state = std::sync::Arc::new(tokio::sync::Mutex::new(
            snapcast_server::state::ServerState::default(),
        ));
        let (notify_tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(256);
        let auth_cfg = std::sync::Arc::new(auth::AuthConfig::default());
        let methods = std::sync::Arc::new(std::collections::HashSet::<String>::new());
        let notifications = std::sync::Arc::new(std::collections::HashSet::<String>::new());

        // Event channel for control servers (separate from library events)
        let (ctrl_event_tx, mut ctrl_event_rx) = tokio::sync::mpsc::channel::<ControlEvent>(256);

        // TCP JSON-RPC control
        let control_cfg = control::ControlConfig {
            port: server_config.control_port,
            state: std::sync::Arc::clone(&shared_state),
            event_tx: ctrl_event_tx.clone(),
            notify_tx: notify_tx.clone(),
            auth_config: std::sync::Arc::clone(&auth_cfg),
            cmd_tx: server.command_sender(),
            registered_methods: std::sync::Arc::clone(&methods),
            registered_notifications: std::sync::Arc::clone(&notifications),
        };
        tokio::spawn(async move {
            if let Err(e) = control::run_tcp(control_cfg).await {
                tracing::error!(error = %e, "Control server error");
            }
        });

        // HTTP/WebSocket + Snapweb
        let http_cfg = http::HttpConfig {
            port: server_config.http_port,
            doc_root: server_config.doc_root.clone(),
            state: std::sync::Arc::clone(&shared_state),
            event_tx: ctrl_event_tx.clone(),
            notify_tx: notify_tx.clone(),
            auth_config: std::sync::Arc::clone(&auth_cfg),
            cmd_tx: server.command_sender(),
        };
        tokio::spawn(async move {
            if let Err(e) = http::run_http(http_cfg).await {
                tracing::error!(error = %e, "HTTP server error");
            }
        });

        // Drain control events (JSON-RPC extension point)
        tokio::spawn(async move {
            while let Some(event) = ctrl_event_rx.recv().await {
                match event {
                    ControlEvent::JsonRpc {
                        client_id, request, ..
                    } => {
                        tracing::debug!(client_id, ?request, "Unhandled JSON-RPC");
                    }
                }
            }
        });

        // Log events
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ServerEvent::ClientConnected { id, name } => {
                        tracing::info!(id, name, "Client connected");
                    }
                    ServerEvent::ClientDisconnected { id } => {
                        tracing::info!(id, "Client disconnected");
                    }
                    ServerEvent::StreamStatus { stream_id, status } => {
                        tracing::info!(stream_id, status, "Stream status");
                    }
                    _ => {}
                }
            }
        });

        server.run().await
    })
}
