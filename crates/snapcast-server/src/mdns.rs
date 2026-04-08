//! mDNS service advertisement for `_snapcast._tcp`.

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};

/// Advertise the Snapcast server via mDNS.
pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl MdnsAdvertiser {
    /// Start advertising on the given port.
    pub fn new(port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let host = hostname::get()?.to_string_lossy().to_string();
        let host = format!(
            "{}.local.",
            host.trim_end_matches(".local")
                .trim_end_matches(".local.")
                .split('.')
                .next()
                .unwrap_or(&host)
        );
        let service =
            ServiceInfo::new("_snapcast._tcp.local.", "Snapserver", &host, "", port, None)?;
        let fullname = service.get_fullname().to_string();
        daemon.register(service)?;
        tracing::info!(port, host, "mDNS: advertising _snapcast._tcp");
        Ok(Self { daemon, fullname })
    }

    /// Graceful shutdown — unregister and stop daemon.
    pub fn shutdown(&self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        // Don't unregister on drop — use shutdown() for clean exit
        let _ = self.daemon.shutdown();
    }
}
