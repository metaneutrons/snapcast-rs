//! mDNS service advertisement.

use anyhow::Result;
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

/// Advertise the Snapcast server via mDNS.
pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
}

impl MdnsAdvertiser {
    /// Start advertising on the given port with the given service type and name.
    pub fn new(port: u16, service_type: &str, service_name: &str) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        // Explicitly enable all interfaces (fixes Docker/container environments
        // where the daemon might only bind to loopback by default).
        daemon.enable_interface(IfKind::All)?;
        let host = hostname::get()?.to_string_lossy().to_string();
        let short = host.split('.').next().unwrap_or(&host);
        let mdns_host = format!("{short}.local.");
        let service = ServiceInfo::new(service_type, service_name, &mdns_host, "", port, None)?
            .enable_addr_auto();
        daemon.register(service)?;
        tracing::info!(port, host = %mdns_host, service_type, "mDNS: advertising");
        Ok(Self { daemon })
    }

    /// Shut down the mDNS daemon.
    pub fn shutdown(&self) {
        let _ = self.daemon.shutdown();
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        self.shutdown();
    }
}
