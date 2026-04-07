use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Snapcast client — synchronized multiroom audio player.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Snapserver URL (tcp://<host>[:port] or ws://<host>[:port])
    #[arg(default_value = "tcp://localhost:1704")]
    url: String,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    tracing::info!(url = %cli.url, "snapclient-rs starting");

    // TODO: implement controller
    Ok(())
}
