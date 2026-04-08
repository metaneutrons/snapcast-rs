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
        let service = ServiceInfo::new(
            "_snapcast._tcp.local.",
            "Snapserver",
            &format!("{}.", hostname::get()?.to_string_lossy()),
            "",
            port,
            None,
        )?;
        let fullname = service.get_fullname().to_string();
        daemon.register(service)?;
        tracing::info!(port, "mDNS: advertising _snapcast._tcp");
        Ok(Self { daemon, fullname })
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
    }
}
