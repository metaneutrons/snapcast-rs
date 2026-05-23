//! mDNS service advertisement.
//!
//! Two backends available via feature gates:
//! - `mdns-os` (default): Uses the OS DNS-SD daemon (Avahi/mDNSResponder/Bonjour)
//!   via `astro-dnssd`. No port conflicts, correct interface selection.
//! - `mdns-embedded`: Standalone Rust implementation via `mdns-sd`.
//!   For embedded targets without an OS mDNS daemon.

use anyhow::Result;

/// Advertise the Snapcast server via mDNS.
///
/// Holds the registration alive until dropped.
pub struct MdnsAdvertiser {
    #[cfg(feature = "mdns-os")]
    _service: astro_dnssd::RegisteredDnsService,
    #[cfg(all(feature = "mdns-embedded", not(feature = "mdns-os")))]
    daemon: mdns_sd::ServiceDaemon,
}

impl MdnsAdvertiser {
    /// Start advertising on the given port with the given service type and name.
    ///
    /// # Errors
    ///
    /// Returns an error if the mDNS daemon cannot be started or the service
    /// cannot be registered.
    pub fn new(port: u16, service_type: &str, service_name: &str) -> Result<Self> {
        #[cfg(feature = "mdns-os")]
        {
            Self::new_os(port, service_type, service_name)
        }
        #[cfg(all(feature = "mdns-embedded", not(feature = "mdns-os")))]
        {
            Self::new_embedded(port, service_type, service_name)
        }
    }

    #[cfg(feature = "mdns-os")]
    fn new_os(port: u16, service_type: &str, service_name: &str) -> Result<Self> {
        // astro-dnssd expects the service type without the trailing ".local."
        let regtype = service_type
            .trim_end_matches("local.")
            .trim_end_matches('.');
        let service = astro_dnssd::DNSServiceBuilder::new(regtype, port)
            .with_name(service_name)
            .register()
            .map_err(|e| anyhow::anyhow!("DNS-SD registration failed: {e}"))?;
        tracing::info!(
            port,
            service_type = regtype,
            name = service_name,
            "mDNS: advertising via OS daemon"
        );
        Ok(Self { _service: service })
    }

    #[cfg(all(feature = "mdns-embedded", not(feature = "mdns-os")))]
    fn new_embedded(port: u16, service_type: &str, service_name: &str) -> Result<Self> {
        let daemon = mdns_sd::ServiceDaemon::new()?;
        let host = hostname::get()?.to_string_lossy().to_string();
        let short = host.split('.').next().unwrap_or(&host);
        let mdns_host = format!("{short}.local.");
        let service =
            mdns_sd::ServiceInfo::new(service_type, service_name, &mdns_host, "", port, None)?
                .enable_addr_auto();
        daemon.register(service)?;
        tracing::info!(port, host = %mdns_host, service_type, "mDNS: advertising via embedded daemon");
        Ok(Self { daemon })
    }
}

#[cfg(all(feature = "mdns-embedded", not(feature = "mdns-os")))]
impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}
