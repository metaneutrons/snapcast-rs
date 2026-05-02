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
    let vol = if muted { 0 } else { percent };
    if let Err(e) = set_alsa_volume_inner(control, vol) {
        tracing::warn!(control, error = %e, "Failed to set ALSA volume");
    } else {
        tracing::debug!(control, percent, muted, "Hardware volume set");
    }
}

#[cfg(target_os = "linux")]
fn set_alsa_volume_inner(control: &str, percent: u8) -> anyhow::Result<()> {
    use alsa::mixer::{Mixer, SelemId};
    let mixer = Mixer::new("default", false)?;
    let selem_id = SelemId::new(control, 0);
    let selem = mixer
        .find_selem(&selem_id)
        .ok_or_else(|| anyhow::anyhow!("ALSA control '{control}' not found"))?;
    let (min, max) = selem.get_playback_volume_range();
    let vol = min + (max - min) * i64::from(percent) / 100;
    selem.set_playback_volume_all(vol)?;
    if selem.has_playback_switch() {
        selem.set_playback_switch_all(if percent == 0 { 0 } else { 1 })?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_alsa_control(control: &str) -> bool {
    use alsa::mixer::{Mixer, SelemId};
    Mixer::new("default", false)
        .ok()
        .and_then(|m| m.find_selem(&SelemId::new(control, 0)))
        .is_some()
}

#[cfg(target_os = "linux")]
fn list_alsa_controls() -> Option<String> {
    use alsa::mixer::{Mixer, Selem};
    let mixer = Mixer::new("default", false).ok()?;
    let names: Vec<String> = mixer
        .iter()
        .filter_map(|elem| {
            let selem = Selem::new(elem)?;
            Some(selem.get_id().get_name().ok()?.to_string())
        })
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
    use alsa::mixer::{Mixer, Selem};
    let mixer = Mixer::new("default", false).ok()?;
    mixer.iter().find_map(|elem| {
        let selem = Selem::new(elem)?;
        if selem.has_playback_volume() {
            Some(selem.get_id().get_name().ok()?.to_string())
        } else {
            None
        }
    })
}
