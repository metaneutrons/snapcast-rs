use clap::Parser;
use snapclient_rs::cli;
use snapclient_rs::controller::Controller;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    // Initialize logging from CLI options before anything else
    snapclient_rs::logging::init(&cli.logsink, &cli.logfilter)?;

    let settings = cli.into_settings()?;
    tracing::info!(
        server = %format!("{}://{}:{}", settings.server.scheme, settings.server.host, settings.server.port),
        instance = settings.instance,
        "snapclient-rs starting"
    );

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut controller = Controller::new(settings);
        controller.run().await
    })
}
