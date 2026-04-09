//! Config file parser for `/etc/snapserver.conf` (INI format).

use ini::Ini;

use snapcast_server::ServerConfig;

/// Parse a snapserver.conf INI file into a [`ServerConfig`].
pub fn parse_config_file(path: &str) -> ServerConfig {
    let mut config = ServerConfig::default();

    let ini = match Ini::load_from_file(path) {
        Ok(ini) => ini,
        Err(e) => {
            tracing::debug!(path, error = %e, "Config file not found, using defaults");
            return config;
        }
    };

    tracing::info!(path, "Loaded config file");

    if let Some(s) = ini.section(Some("server")) {
        get_str(s, "datadir", |v| {
            config.state_file = Some(format!("{v}/server.json"));
        });
    }

    if let Some(s) = ini.section(Some("http")) {
        get_u16(s, "port", |v| config.http_port = v);
        get_str(s, "doc_root", |v| config.doc_root = Some(v.to_string()));
    }

    if let Some(s) = ini.section(Some("tcp-control")) {
        get_u16(s, "port", |v| config.control_port = v);
    }

    if let Some(s) = ini.section(Some("tcp-streaming")) {
        get_u16(s, "port", |v| config.stream_port = v);
    }

    if let Some(s) = ini.section(Some("stream")) {
        let sources: Vec<String> = s.get_all("source").map(String::from).collect();
        if !sources.is_empty() {
            config.sources = sources;
        }
        get_str(s, "codec", |v| config.codec = v.to_string());
        get_str(s, "sampleformat", |v| config.sample_format = v.to_string());
        get_u32(s, "buffer", |v| config.buffer_ms = v);
    }

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

/// CLI overrides for server config.
pub struct CliOverrides {
    /// Override stream port.
    pub stream_port: Option<u16>,
    /// Override control port.
    pub control_port: Option<u16>,
    /// Override HTTP port.
    pub http_port: Option<u16>,
    /// Override Snapweb doc root.
    pub doc_root: Option<String>,
    /// Override buffer size.
    pub buffer: Option<u32>,
    /// Override codec.
    pub codec: Option<String>,
    /// Override sample format.
    pub sampleformat: Option<String>,
    /// Override stream sources.
    pub sources: Vec<String>,
}

/// Merge CLI overrides into a config. Non-default CLI values take precedence.
pub fn merge_cli(mut config: ServerConfig, cli: CliOverrides) -> ServerConfig {
    if let Some(v) = cli.stream_port {
        config.stream_port = v;
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
        config.buffer_ms = v;
    }
    if let Some(v) = cli.codec {
        config.codec = v;
    }
    if let Some(v) = cli.sampleformat {
        config.sample_format = v;
    }
    if !cli.sources.is_empty() {
        config.sources = cli.sources;
    }
    config
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
        assert_eq!(config.stream_port, 2704);
    }

    #[test]
    fn missing_file_returns_defaults() {
        let config = parse_config_file("/nonexistent/snapserver.conf");
        assert_eq!(config.stream_port, 1704);
    }

    #[test]
    fn merge_cli_overrides() {
        let config = ServerConfig::default();
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
            },
        );
        assert_eq!(merged.stream_port, 9704);
        assert_eq!(merged.control_port, 1705);
    }
}
