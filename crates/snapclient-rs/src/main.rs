use clap::Parser;
use snapclient_rs::cli;
use snapclient_rs::controller::Controller;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    snapclient_rs::logging::init(&cli.logsink, &cli.logfilter)?;

    // Handle --list: list PCM devices and exit
    if cli.list {
        list_devices(&cli.player);
        return Ok(());
    }

    let settings = cli.into_settings()?;

    // Handle daemon mode
    #[cfg(unix)]
    if let Some(ref daemon) = settings.daemon {
        daemonize(daemon)?;
    }

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

fn list_devices(player: &str) {
    let player_name = player.split(':').next().unwrap_or("");
    match player_name {
        #[cfg(feature = "alsa")]
        "alsa" | "" => match snapclient_rs::player::alsa::AlsaPlayer::pcm_list() {
            Ok(devices) => {
                for dev in &devices {
                    println!("{}: {}\n{}\n", dev.idx, dev.name, dev.description);
                }
                if devices.is_empty() {
                    println!("No PCM devices found for 'alsa'");
                }
            }
            Err(e) => println!("Failed to list devices: {e}"),
        },
        #[cfg(feature = "coreaudio")]
        "coreaudio" | "" => {
            println!("0: Default Output\nCoreAudio default output device\n");
        }
        _ => println!("No device listing available for '{player_name}'"),
    }
}

#[cfg(unix)]
fn daemonize(daemon: &snapclient_rs::config::DaemonSettings) -> anyhow::Result<()> {
    if let Some(priority) = daemon.priority {
        // setpriority for current process
        let priority = priority.clamp(-20, 19);
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, 0, priority);
        }
        tracing::info!(priority, "Process priority set");
    }

    if let Some(ref user) = daemon.user {
        // Drop privileges — for now just log, full impl needs nix crate
        tracing::info!(user, "Would drop privileges to user (not yet implemented)");
    }

    // Fork to background
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            anyhow::bail!("fork failed");
        }
        if pid > 0 {
            // Parent exits
            std::process::exit(0);
        }
        // Child continues
        libc::setsid();
    }

    tracing::info!("Daemonized");
    Ok(())
}
