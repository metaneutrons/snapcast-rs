use clap::Parser;
use snapclient_rs::cli;
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
