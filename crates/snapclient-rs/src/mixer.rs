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
    // Perceptual volume curve (quadratic) — matches how humans perceive loudness.
    let normalized = f64::from(percent) / 100.0;
    let curved = normalized * normalized * normalized;
    let vol = min + ((max - min) as f64 * curved) as i64;
    selem.set_playback_volume_all(vol)?;
    if selem.has_playback_switch() {
        selem.set_playback_switch_all(if percent == 0 { 0 } else { 1 })?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_alsa_control(control: &str) -> bool {
    use alsa::mixer::{Mixer, SelemId};
    let Ok(mixer) = Mixer::new("default", false) else {
        return false;
    };
    mixer.find_selem(&SelemId::new(control, 0)).is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- VolumeState ----

    #[test]
    fn volume_state_new_defaults() {
        let vol = VolumeState::new();
        assert_eq!(vol.percent.load(Ordering::Relaxed), 100);
        assert!(!vol.muted.load(Ordering::Relaxed));
    }

    #[test]
    fn gain_default_is_unity() {
        let vol = VolumeState::new();
        assert_eq!(vol.gain(), 1.0);
    }

    #[test]
    fn gain_scales_linearly_with_percent() {
        let vol = VolumeState::new();
        vol.percent.store(50, Ordering::Relaxed);
        assert_eq!(vol.gain(), 0.5);

        vol.percent.store(25, Ordering::Relaxed);
        assert_eq!(vol.gain(), 0.25);

        vol.percent.store(0, Ordering::Relaxed);
        assert_eq!(vol.gain(), 0.0);
    }

    #[test]
    fn gain_full_scale_at_100() {
        let vol = VolumeState::new();
        vol.percent.store(100, Ordering::Relaxed);
        assert_eq!(vol.gain(), 1.0);
    }

    #[test]
    fn gain_muted_is_zero_regardless_of_percent() {
        let vol = VolumeState::new();
        // A non-zero percent that would otherwise produce audible gain.
        vol.percent.store(80, Ordering::Relaxed);
        vol.muted.store(true, Ordering::Relaxed);
        assert_eq!(vol.gain(), 0.0);

        // Un-muting restores the underlying percent-derived gain.
        vol.muted.store(false, Ordering::Relaxed);
        assert_eq!(vol.gain(), 0.8);
    }

    #[test]
    fn gain_matches_percent_over_full_range() {
        let vol = VolumeState::new();
        for p in 0u8..=100 {
            vol.percent.store(p, Ordering::Relaxed);
            let expected = p as f32 / 100.0;
            assert!(
                (vol.gain() - expected).abs() < f32::EPSILON,
                "percent {p} -> gain {} != {expected}",
                vol.gain()
            );
        }
    }

    // ---- Mixer::from_str parsing ----

    #[test]
    fn from_str_software_selects_software_backend() {
        let (mixer, _vol) = Mixer::from_str("software");
        assert!(matches!(mixer, Mixer::Software(_)));
    }

    #[test]
    fn from_str_empty_defaults_to_software() {
        let (mixer, _vol) = Mixer::from_str("");
        assert!(matches!(mixer, Mixer::Software(_)));
    }

    #[test]
    fn from_str_none_selects_no_control() {
        let (mixer, _vol) = Mixer::from_str("none");
        assert!(matches!(mixer, Mixer::None));
    }

    #[test]
    fn from_str_unknown_mode_falls_back_to_software() {
        let (mixer, _vol) = Mixer::from_str("bogus");
        assert!(matches!(mixer, Mixer::Software(_)));
    }

    #[test]
    fn from_str_splits_on_colon_and_ignores_param_for_software() {
        // `split_once(':')` means the mode is only the part before the colon;
        // "software:whatever" is still the software backend.
        let (mixer, _vol) = Mixer::from_str("software:ignored");
        assert!(matches!(mixer, Mixer::Software(_)));
    }

    #[test]
    fn from_str_none_with_colon_param_is_still_none() {
        let (mixer, _vol) = Mixer::from_str("none:whatever");
        assert!(matches!(mixer, Mixer::None));
    }

    // On non-Linux targets the ALSA backend is compiled out, so "hardware"
    // deterministically falls back to the software mixer (no hardware probe).
    #[test]
    #[cfg(not(target_os = "linux"))]
    fn from_str_hardware_falls_back_to_software_off_linux() {
        let (mixer, _vol) = Mixer::from_str("hardware");
        assert!(matches!(mixer, Mixer::Software(_)));

        // A named control is likewise ignored off-Linux.
        let (mixer2, _vol2) = Mixer::from_str("hardware:Master");
        assert!(matches!(mixer2, Mixer::Software(_)));
    }

    // ---- from_str returned VolumeState handle ----

    #[test]
    fn from_str_returns_default_volume_handle() {
        let (_mixer, vol) = Mixer::from_str("software");
        assert_eq!(vol.percent.load(Ordering::Relaxed), 100);
        assert!(!vol.muted.load(Ordering::Relaxed));
    }

    #[test]
    fn from_str_software_handle_is_shared_with_backend() {
        // The returned handle must alias the Arc stored inside the Software
        // variant, otherwise set_volume() would update invisible state.
        let (mixer, vol) = Mixer::from_str("software");
        match &mixer {
            Mixer::Software(inner) => {
                assert!(
                    Arc::ptr_eq(inner, &vol),
                    "returned handle must be the backend's Arc"
                );
            }
            _ => panic!("expected software backend"),
        }
    }

    // ---- Mixer::set_volume dispatch ----

    #[test]
    fn set_volume_software_updates_shared_state() {
        let (mixer, vol) = Mixer::from_str("software");
        mixer.set_volume(75, false);
        assert_eq!(vol.percent.load(Ordering::Relaxed), 75);
        assert!(!vol.muted.load(Ordering::Relaxed));

        // Observed through gain(): 75% unmuted -> 0.75.
        assert_eq!(vol.gain(), 0.75);
    }

    #[test]
    fn set_volume_software_applies_mute() {
        let (mixer, vol) = Mixer::from_str("software");
        mixer.set_volume(60, true);
        assert_eq!(vol.percent.load(Ordering::Relaxed), 60);
        assert!(vol.muted.load(Ordering::Relaxed));
        // Muted overrides percent in the gain calculation.
        assert_eq!(vol.gain(), 0.0);
    }

    #[test]
    fn set_volume_software_boundary_values() {
        let (mixer, vol) = Mixer::from_str("software");

        mixer.set_volume(0, false);
        assert_eq!(vol.percent.load(Ordering::Relaxed), 0);
        assert_eq!(vol.gain(), 0.0);

        mixer.set_volume(100, false);
        assert_eq!(vol.percent.load(Ordering::Relaxed), 100);
        assert_eq!(vol.gain(), 1.0);
    }

    #[test]
    fn set_volume_software_last_write_wins() {
        let (mixer, vol) = Mixer::from_str("software");
        mixer.set_volume(30, true);
        mixer.set_volume(90, false);
        assert_eq!(vol.percent.load(Ordering::Relaxed), 90);
        assert!(!vol.muted.load(Ordering::Relaxed));
        assert_eq!(vol.gain(), 0.9);
    }

    #[test]
    fn set_volume_none_is_a_noop_on_returned_handle() {
        // For the `none` backend the returned handle is a fresh default state
        // that set_volume never touches.
        let (mixer, vol) = Mixer::from_str("none");
        mixer.set_volume(10, true);
        assert_eq!(vol.percent.load(Ordering::Relaxed), 100);
        assert!(!vol.muted.load(Ordering::Relaxed));
        assert_eq!(vol.gain(), 1.0);
    }
}
