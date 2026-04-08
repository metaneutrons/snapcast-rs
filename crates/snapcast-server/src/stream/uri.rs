//! Stream URI parser matching C++ `source=` config syntax.
//!
//! Format: `scheme:///path?key=value&key=value`
//! Examples:
//! - `pipe:///tmp/snapfifo?name=Radio&sampleformat=48000:16:2`
//! - `process:///usr/bin/mpd?name=MPD`
//! - `tcp://0.0.0.0:4953?name=TCP`
//! - `file:///path/to/file.wav?name=File`

use std::collections::HashMap;

use anyhow::{Context, Result};

/// Parsed stream URI.
#[derive(Debug, Clone)]
pub struct StreamUri {
    /// Scheme: pipe, file, process, tcp, alsa, librespot, airplay, meta, etc.
    pub scheme: String,
    /// Host (for tcp scheme).
    pub host: String,
    /// Port (for tcp scheme).
    pub port: u16,
    /// Path (for pipe, file, process schemes).
    pub path: String,
    /// Query parameters.
    pub query: HashMap<String, String>,
}

impl StreamUri {
    /// Parse a stream URI string.
    pub fn parse(uri: &str) -> Result<Self> {
        let uri = uri.trim().trim_matches(|c| c == '\'' || c == '"');

        let (scheme, rest) = uri
            .split_once("://")
            .with_context(|| format!("invalid stream URI: {uri}"))?;

        // Split path and query
        let (path_part, query_str) = rest.split_once('?').unwrap_or((rest, ""));

        // Parse query parameters
        let mut query = HashMap::new();
        for pair in query_str.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                // URL-decode %XX sequences
                let v = url_decode(v);
                query.insert(k.to_string(), v);
            }
        }

        // Parse host:port for tcp scheme
        let (host, port, path) = if scheme == "tcp" {
            let (hp, p) = if let Some(idx) = path_part.rfind(':') {
                let port: u16 = path_part[idx + 1..].parse().unwrap_or(4953);
                (path_part[..idx].to_string(), port)
            } else {
                (path_part.to_string(), 4953)
            };
            // Strip leading slashes from host
            let host = hp.trim_start_matches('/').to_string();
            (host, p, String::new())
        } else {
            // For pipe/file/process: path starts after ://
            // Typically pipe:///tmp/snapfifo → path = /tmp/snapfifo
            let path = url_decode(path_part.strip_prefix("//").unwrap_or(path_part));
            (String::new(), 0, path)
        };

        Ok(Self {
            scheme: scheme.to_string(),
            host,
            port,
            path,
            query,
        })
    }

    /// Get a query parameter value.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(|s| s.as_str())
    }
}

fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            let val = hex_val(hi) * 16 + hex_val(lo);
            result.push(val as char);
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pipe_uri() {
        let u =
            StreamUri::parse("pipe:///tmp/snapfifo?name=Radio&sampleformat=48000:16:2").unwrap();
        assert_eq!(u.scheme, "pipe");
        assert_eq!(u.path, "/tmp/snapfifo");
        assert_eq!(u.param("name"), Some("Radio"));
        assert_eq!(u.param("sampleformat"), Some("48000:16:2"));
    }

    #[test]
    fn parse_tcp_uri() {
        let u = StreamUri::parse("tcp://0.0.0.0:4953?name=TCP").unwrap();
        assert_eq!(u.scheme, "tcp");
        assert_eq!(u.host, "0.0.0.0");
        assert_eq!(u.port, 4953);
        assert_eq!(u.param("name"), Some("TCP"));
    }

    #[test]
    fn parse_file_uri_with_spaces() {
        let u =
            StreamUri::parse("file:///home/user/Musik/Some%20wave%20file.wav?name=File").unwrap();
        assert_eq!(u.scheme, "file");
        assert_eq!(u.path, "/home/user/Musik/Some wave file.wav");
        assert_eq!(u.param("name"), Some("File"));
    }

    #[test]
    fn parse_process_uri() {
        let u =
            StreamUri::parse("process:///usr/bin/mpd?name=MPD&sampleformat=44100:16:2").unwrap();
        assert_eq!(u.scheme, "process");
        assert_eq!(u.path, "/usr/bin/mpd");
    }
}
