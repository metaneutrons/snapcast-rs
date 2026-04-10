//! mDNS/ZeroConf service discovery for finding snapservers.

use std::time::Duration;

use anyhow::{Result, bail};

/// Discovered snapserver endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Hostname or IP address.
    pub host: String,
    /// TCP port number.
    pub port: u16,
}

/// Browse for a snapserver via mDNS. Returns the first resolved endpoint.
pub async fn discover(timeout: Duration, service_type: &str) -> Result<Endpoint> {
    let mdns = mdns_sd::ServiceDaemon::new()?;
    let receiver = mdns.browse(service_type)?;

    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            mdns.stop_browse(service_type).ok();
            bail!("mDNS discovery timed out after {timeout:?}");
        }

        match tokio::time::timeout(remaining, async { receiver.recv_async().await }).await {
            Ok(Ok(mdns_sd::ServiceEvent::ServiceResolved(info))) => {
                let host = info
                    .get_addresses()
                    .iter()
                    .next()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| info.get_hostname().trim_end_matches('.').to_string());
                let port = info.get_port();
                tracing::info!(host = %host, port, "Discovered snapserver via mDNS");
                mdns.stop_browse(service_type).ok();
                return Ok(Endpoint { host, port });
            }
            Ok(Ok(_)) => continue, // other events, keep waiting
            Ok(Err(e)) => {
                mdns.stop_browse(service_type).ok();
                bail!("mDNS receiver error: {e}");
            }
            Err(_) => {
                mdns.stop_browse(service_type).ok();
                bail!("mDNS discovery timed out after {timeout:?}");
            }
        }
    }
}
