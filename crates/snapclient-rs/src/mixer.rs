//! Volume mixer — software (PCM scaling) or hardware (ALSA control).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Shared volume state for the software mixer.
pub struct VolumeState {
    pub percent: AtomicU8,
    pub muted: AtomicBool,
}

impl VolumeState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            percent: AtomicU8::new(100),
            muted: AtomicBool::new(false),
        })
    }

    /// Get the linear gain factor (0.0–1.0).
    pub fn gain(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            self.percent.load(Ordering::Relaxed) as f32 / 100.0
        }
    }
}

/// Mixer backend.
pub enum Mixer {
    /// PCM amplitude scaling (default).
    Software(Arc<VolumeState>),
    /// ALSA hardware mixer control (Linux only).
    #[cfg(target_os = "linux")]
    Hardware { control: String },
    /// No volume control.
    None,
}

impl Mixer {
    /// Parse from CLI string: `software`, `hardware[:control]`, `none`.
    pub fn from_str(raw: &str) -> (Self, Arc<VolumeState>) {
        let volume = VolumeState::new();
        #[allow(unused_variables)]
        let (mode, param) = raw.split_once(':').unwrap_or((raw, ""));
        let mixer = match mode {
            "software" | "" => Mixer::Software(volume.clone()),
            #[cfg(target_os = "linux")]
            "hardware" => {
                let control = if param.is_empty() {
                    detect_alsa_control().unwrap_or_else(|| "Master".to_string())
                } else {
                    param.to_string()
                };
                if !validate_alsa_control(&control) {
                    tracing::warn!(
                        control,
                        available = list_alsa_controls().as_deref().unwrap_or("none"),
                        "ALSA mixer control not found"
                    );
                } else {
                    tracing::info!(control, "Hardware mixer initialized");
                }
                Mixer::Hardware { control }
            }
            #[cfg(not(target_os = "linux"))]
            "hardware" => {
                tracing::warn!("Hardware mixer not supported on this platform, using software");
                Mixer::Software(volume.clone())
            }
            "none" => Mixer::None,
            _ => {
                tracing::warn!(mode, "Unknown mixer mode, using software");
                Mixer::Software(volume.clone())
            }
        };
        (mixer, volume)
    }

    /// Apply a volume change from the server.
    pub fn set_volume(&self, percent: u8, muted: bool) {
        match self {
            Mixer::Software(vol) => {
                vol.percent.store(percent, Ordering::Relaxed);
                vol.muted.store(muted, Ordering::Relaxed);
            }
            #[cfg(target_os = "linux")]
            Mixer::Hardware { control } => {
                set_alsa_volume(control, percent, muted);
            }
            Mixer::None => {}
        }
    }
}

#[cfg(target_os = "linux")]
fn set_alsa_volume(control: &str, percent: u8, muted: bool) {
    use std::process::Command;
    let vol_arg = if muted {
        "0%".to_string()
    } else {
        format!("{percent}%")
    };
    match Command::new("amixer")
        .args(["sset", control, &vol_arg])
        .output()
    {
        Ok(output) if !output.status.success() => {
            tracing::warn!(
                control,
                "amixer failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Err(e) => tracing::warn!(control, error = %e, "Failed to run amixer"),
        _ => tracing::debug!(control, percent, muted, "Hardware volume set"),
    }
}

#[cfg(target_os = "linux")]
fn validate_alsa_control(control: &str) -> bool {
    use std::process::Command;
    Command::new("amixer")
        .args(["sget", control])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(target_os = "linux")]
fn list_alsa_controls() -> Option<String> {
    use std::process::Command;
    let output = Command::new("amixer").arg("scontrols").output().ok()?;
    let names: Vec<&str> = std::str::from_utf8(&output.stdout)
        .ok()?
        .lines()
        .filter_map(|l| l.split('\'').nth(1))
        .collect();
    Some(names.join(", "))
}

#[cfg(target_os = "linux")]
fn detect_alsa_control() -> Option<String> {
    for candidate in ["Master", "Digital", "PCM", "Speaker"] {
        if validate_alsa_control(candidate) {
            return Some(candidate.to_string());
        }
    }
    use std::process::Command;
    let output = Command::new("amixer").arg("scontrols").output().ok()?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .lines()
        .next()?
        .split('\'')
        .nth(1)
        .map(String::from)
}
