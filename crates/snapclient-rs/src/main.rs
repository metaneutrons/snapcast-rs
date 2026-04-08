use clap::Parser;
use snapclient_rs::cli;
use snapclient_rs::controller::Controller;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

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

        tokio::select! {
            result = controller.run() => result,
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received Ctrl-C, shutting down");
                controller.shutdown();
                Ok(())
            }
        }
    })?;

    tracing::info!("snapclient-rs terminated");
    Ok(())
}
