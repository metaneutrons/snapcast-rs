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
        let short = host.split('.').next().unwrap_or(&host);
        let mdns_host = format!("{short}.local.");
        let service = ServiceInfo::new(
            "_snapcast._tcp.local.",
            "Snapserver",
            &mdns_host,
            "",
            port,
            None,
        )?;
        let fullname = service.get_fullname().to_string();
        daemon.register(service)?;
        tracing::info!(port, host = %mdns_host, "mDNS: advertising _snapcast._tcp");
        Ok(Self { daemon, fullname })
    }

    /// Gracefully unregister and shut down the mDNS daemon.
    pub fn shutdown(&self) {
        let _ = self.daemon.unregister(&self.fullname);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = self.daemon.shutdown();
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        self.shutdown();
    }
}
