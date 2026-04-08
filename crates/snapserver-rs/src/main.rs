use clap::Parser;
use snapcast_server::{ServerCommand, ServerConfig, ServerEvent, SnapServer};

/// Snapcast server — synchronized multiroom audio server.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// TCP port for binary protocol (client connections)
    #[arg(long, default_value_t = 1704)]
    stream_port: u16,

    /// TCP port for JSON-RPC control
    #[arg(long, default_value_t = 1705)]
    control_port: u16,

    /// HTTP port for JSON-RPC + Snapweb
    #[arg(long, default_value_t = 1780)]
    http_port: u16,

    /// Path to Snapweb static files
    #[arg(long)]
    doc_root: Option<String>,

    /// Audio buffer size in milliseconds
    #[arg(long, default_value_t = 1000)]
    buffer: u32,

    /// Default codec: flac, pcm, opus, ogg
    #[arg(long, default_value = "flac")]
    codec: String,

    /// Default sample format
    #[arg(long, default_value = "48000:16:2")]
    sampleformat: String,

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

    let sources = if cli.sources.is_empty() {
        vec!["pipe:///tmp/snapfifo?name=default".into()]
    } else {
        cli.sources
    };

    let config = ServerConfig {
        stream_port: cli.stream_port,
        control_port: cli.control_port,
        http_port: cli.http_port,
        doc_root: cli.doc_root,
        buffer_ms: cli.buffer,
        codec: cli.codec,
        sample_format: cli.sampleformat,
        sources,
        state_file: Some("/var/lib/snapserver/server.json".into()),
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (mut server, mut events) = SnapServer::new(config);
        let cmd = server.command_sender();

        // Log events + handle custom JSON-RPC (EQ extension point)
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
                    ServerEvent::JsonRpc { client_id, request } => {
                        // Extension point: handle custom JSON-RPC methods here
                        tracing::debug!(client_id, ?request, "Unhandled JSON-RPC");
                    }
                }
            }
        });

        // Ctrl-C
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received Ctrl-C, shutting down");
            cmd.send(ServerCommand::Stop).await.ok();
        });

        server.run().await
    })
}
