//! Config file parser for `/etc/snapserver.conf` (INI format).

use ini::Ini;

use snapcast_server::ServerConfig;

/// Binary-specific configuration (not part of the library).
pub(crate) struct BinaryConfig {
    /// Library server config.
    pub server: ServerConfig,
    /// TCP port for JSON-RPC control. Default: 1705.
    pub control_port: u16,
    /// HTTP port for JSON-RPC + Snapweb. Default: 1780.
    pub http_port: u16,
    /// Path to Snapweb static files (None = disabled).
    pub doc_root: Option<String>,
    /// Stream source URIs.
    pub sources: Vec<String>,
}

impl Default for BinaryConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            control_port: snapcast_proto::DEFAULT_CONTROL_PORT,
            http_port: snapcast_proto::DEFAULT_HTTP_PORT,
            doc_root: None,
            sources: vec!["pipe:///tmp/snapfifo?name=default".into()],
        }
    }
}

/// Parse a snapserver.conf INI file into a [`BinaryConfig`].
pub(crate) fn parse_config_file(path: &str) -> BinaryConfig {
    let mut config = BinaryConfig::default();

    let ini = match Ini::load_from_file(path) {
        Ok(ini) => ini,
        Err(e) => {
            tracing::debug!(path, error = %e, "Config file not found, using defaults");
            return config;
        }
    };

    tracing::info!(path, "Loaded config file");

    if let Some(s) = ini.section(Some("http")) {
        get_u16(s, "port", |v| config.http_port = v);
        get_str(s, "doc_root", |v| config.doc_root = Some(v.to_string()));
    }

    if let Some(s) = ini.section(Some("tcp-control")) {
        get_u16(s, "port", |v| config.control_port = v);
    }

    if let Some(s) = ini.section(Some("tcp-streaming")) {
        get_u16(s, "port", |v| config.server.stream_port = v);
    }

    if let Some(s) = ini.section(Some("stream")) {
        let sources: Vec<String> = s.get_all("source").map(String::from).collect();
        if !sources.is_empty() {
            config.sources = sources;
        }
        get_str(s, "codec", |v| config.server.codec = v.to_string());
        get_str(s, "sampleformat", |v| {
            config.server.sample_format = v.to_string();
        });
        get_u32(s, "buffer", |v| config.server.buffer_ms = v);
        #[cfg(feature = "encryption")]
        get_str(s, "encryption_psk", |v| {
            config.server.encryption_psk = Some(v.to_string());
        });
    }

    resolve_encryption(&mut config);

    config
}

fn get_str<F: FnOnce(&str)>(section: &ini::Properties, key: &str, f: F) {
    if let Some(v) = section.get(key) {
        f(v);
    }
}

fn get_u16<F: FnOnce(u16)>(section: &ini::Properties, key: &str, f: F) {
    if let Some(v) = section.get(key).and_then(|v| v.parse().ok()) {
        f(v);
    }
}

fn get_u32<F: FnOnce(u32)>(section: &ini::Properties, key: &str, f: F) {
    if let Some(v) = section.get(key).and_then(|v| v.parse().ok()) {
        f(v);
    }
}

/// CLI overrides.
pub(crate) struct CliOverrides {
    pub stream_port: Option<u16>,
    pub control_port: Option<u16>,
    pub http_port: Option<u16>,
    pub doc_root: Option<String>,
    pub buffer: Option<u32>,
    pub codec: Option<String>,
    pub sampleformat: Option<String>,
    pub sources: Vec<String>,
    #[cfg(feature = "encryption")]
    pub encryption_psk: Option<String>,
    #[cfg(feature = "mdns")]
    pub no_mdns: bool,
    #[cfg(feature = "mdns")]
    pub mdns_name: Option<String>,
}

/// Merge CLI overrides into config.
pub(crate) fn merge_cli(mut config: BinaryConfig, cli: CliOverrides) -> BinaryConfig {
    if let Some(v) = cli.stream_port {
        config.server.stream_port = v;
    }
    if let Some(v) = cli.control_port {
        config.control_port = v;
    }
    if let Some(v) = cli.http_port {
        config.http_port = v;
    }
    if let Some(v) = cli.doc_root {
        config.doc_root = Some(v);
    }
    if let Some(v) = cli.buffer {
        config.server.buffer_ms = v;
    }
    if let Some(v) = cli.codec {
        config.server.codec = v;
    }
    if let Some(v) = cli.sampleformat {
        config.server.sample_format = v;
    }
    if !cli.sources.is_empty() {
        config.sources = cli.sources;
    }
    #[cfg(feature = "encryption")]
    if let Some(v) = cli.encryption_psk {
        config.server.encryption_psk = Some(v);
    }
    #[cfg(feature = "mdns")]
    {
        if cli.no_mdns {
            config.server.mdns_enabled = false;
        }
        if let Some(v) = cli.mdns_name {
            config.server.mdns_name = v;
        }
    }

    // Resolve f32lz4e → f32lz4 + default PSK (if no explicit PSK set)
    resolve_encryption(&mut config);

    config
}

/// If codec is `f32lz4e`, rewrite to `f32lz4` and apply default PSK
/// unless an explicit PSK was already set.
#[cfg(feature = "encryption")]
fn resolve_encryption(config: &mut BinaryConfig) {
    if config.server.codec == "f32lz4e" {
        config.server.codec = "f32lz4".into();
        if config.server.encryption_psk.is_none() {
            config.server.encryption_psk = Some(snapcast_proto::DEFAULT_ENCRYPTION_PSK.into());
        }
    }
}

#[cfg(not(feature = "encryption"))]
fn resolve_encryption(config: &mut BinaryConfig) {
    if config.server.codec == "f32lz4e" {
        tracing::error!("Codec f32lz4e requires the 'encryption' feature — falling back to f32lz4");
        config.server.codec = "f32lz4".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_minimal_config() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "[stream]\nsource = pipe:///tmp/snapfifo?name=test\n\n[http]\nport = 8080\ndoc_root = /var/www\n\n[tcp-streaming]\nport = 2704"
        )
        .unwrap();

        let config = parse_config_file(tmp.path().to_str().unwrap());
        assert_eq!(config.sources, vec!["pipe:///tmp/snapfifo?name=test"]);
        assert_eq!(config.http_port, 8080);
        assert_eq!(config.doc_root, Some("/var/www".into()));
        assert_eq!(config.server.stream_port, 2704);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let config = parse_config_file("/nonexistent/snapserver.conf");
        assert_eq!(config.server.stream_port, 1704);
    }

    #[test]
    fn merge_cli_overrides() {
        let config = BinaryConfig::default();
        let merged = merge_cli(
            config,
            CliOverrides {
                stream_port: Some(9704),
                control_port: None,
                http_port: None,
                doc_root: None,
                buffer: None,
                codec: None,
                sampleformat: None,
                sources: vec![],
                #[cfg(feature = "mdns")]
                no_mdns: false,
                #[cfg(feature = "mdns")]
                mdns_name: None,
            },
        );
        assert_eq!(merged.server.stream_port, 9704);
        assert_eq!(merged.control_port, 1705);
    }
}
