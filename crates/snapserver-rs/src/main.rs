use clap::Parser;
use snapcast_server::{ServerCommand, ServerEvent, SnapServer};

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

    /// Default codec: flac, pcm, opus, ogg
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
    let file_config = snapcast_server::config::parse_config_file(&cli.config);
    let config = snapcast_server::config::merge_cli(
        file_config,
        snapcast_server::config::CliOverrides {
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

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (mut server, mut events) = SnapServer::new(config);
        let cmd = server.command_sender();

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
                        tracing::debug!(client_id, ?request, "Unhandled JSON-RPC");
                    }
                }
            }
        });

        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received Ctrl-C, shutting down");
            cmd.send(ServerCommand::Stop).await.ok();
        });

        server.run().await
    })
}
