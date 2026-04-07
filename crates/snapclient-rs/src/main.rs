mod cli;
mod config;
#[allow(dead_code)]
mod connection;
#[allow(dead_code)]
mod decoder;
#[allow(dead_code)]
mod double_buffer;
#[allow(dead_code)]
mod time_provider;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let settings = cli.into_settings()?;
    tracing::info!(
        server = %format!("{}://{}:{}", settings.server.scheme, settings.server.host, settings.server.port),
        instance = settings.instance,
        "snapclient-rs starting"
    );

    // TODO: implement controller
    Ok(())
}
