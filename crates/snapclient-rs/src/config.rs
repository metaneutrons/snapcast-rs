//! Client configuration types mirroring the C++ `ClientSettings`.

use std::path::PathBuf;

use snapcast_proto::SampleFormat;

/// Server connection settings.
#[derive(Debug, Clone)]
pub struct ServerSettings {
    /// Connection scheme: "tcp", "ws", or "wss".
    pub scheme: String,
    /// Server hostname or IP.
    pub host: String,
    /// Server port.
    pub port: u16,
    /// Optional authentication.
    pub auth: Option<Auth>,
    /// Server CA certificate for TLS verification.
    pub server_certificate: Option<PathBuf>,
    /// Client certificate (PEM).
    pub certificate: Option<PathBuf>,
    /// Client private key (PEM).
    pub certificate_key: Option<PathBuf>,
    /// Password for encrypted private key.
    pub key_password: Option<String>,
}

/// Authentication credentials.
#[derive(Debug, Clone)]
pub struct Auth {
    pub scheme: String,
    pub param: String,
}

/// Mixer mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MixerMode {
    #[default]
    Software,
    Hardware,
    Script,
    None,
}

/// Mixer settings.
#[derive(Debug, Clone, Default)]
pub struct MixerSettings {
    pub mode: MixerMode,
    pub parameter: String,
}

/// PCM device identifier.
#[derive(Debug, Clone)]
pub struct PcmDevice {
    pub idx: i32,
    pub name: String,
    pub description: String,
}

impl Default for PcmDevice {
    fn default() -> Self {
        Self {
            idx: -1,
            name: "default".into(),
            description: String::new(),
        }
    }
}

/// Audio player settings.
#[derive(Debug, Clone, Default)]
pub struct PlayerSettings {
    pub player_name: String,
    pub parameter: String,
    pub latency: i32,
    pub pcm_device: PcmDevice,
    pub sample_format: SampleFormat,
    pub mixer: MixerSettings,
}

/// Logging settings.
#[derive(Debug, Clone)]
pub struct LoggingSettings {
    pub sink: String,
    pub filter: String,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            sink: "stdout".into(),
            filter: "*:info".into(),
        }
    }
}

/// Top-level client settings.
#[derive(Debug, Clone)]
pub struct ClientSettings {
    pub instance: u32,
    pub host_id: String,
    pub server: ServerSettings,
    pub player: PlayerSettings,
    pub logging: LoggingSettings,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            scheme: "tcp".into(),
            host: String::new(),
            port: 1704,
            auth: None,
            server_certificate: None,
            certificate: None,
            certificate_key: None,
            key_password: None,
        }
    }
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            instance: 1,
            host_id: String::new(),
            server: ServerSettings::default(),
            player: PlayerSettings::default(),
            logging: LoggingSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_cpp() {
        let s = ClientSettings::default();
        assert_eq!(s.instance, 1);
        assert_eq!(s.server.scheme, "tcp");
        assert_eq!(s.server.port, 1704);
        assert_eq!(s.player.latency, 0);
        assert_eq!(s.player.pcm_device.name, "default");
        assert_eq!(s.player.mixer.mode, MixerMode::Software);
        assert_eq!(s.logging.filter, "*:info");
    }
}
