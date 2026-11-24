use crate::audio;
use crate::chatmix::ChatMixSettings;
use crate::mic::MicSettings;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EqPreset {
    pub name: String,
    pub bands: [f32; 10],
}

/// What the app last applied. The headset reports lights, auto-off and noise
/// control back (those are read live); sidetone, EQ and mic gain it does not,
/// so they are restored from here. Stored in
/// $XDG_CONFIG_HOME/arctis-control/settings.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub sidetone: u8,
    pub lights: u8,
    pub inactive: u8,
    pub noise_control: u8,

    pub bands: [f32; 10],
    /// Name of the curve in `bands` ("" when edited by hand).
    pub eq_name: String,
    /// Sends a flat curve to the headset without forgetting `bands`.
    pub eq_bypass: bool,
    pub eq_user_presets: Vec<EqPreset>,

    pub mic: MicSettings,
    pub chatmix: ChatMixSettings,

    pub low_battery_percent: u8,
    pub notify_spare_ready: bool,
    /// Learned seconds per battery gauge step while discharging.
    pub secs_per_step: f32,
}

/// Arctis Nova Pro Wireless batteries are rated for roughly 18-22 h; until
/// the app has watched one drain, assume 20 h over 8 gauge steps.
pub const NOMINAL_SECS_PER_STEP: f32 = 20.0 * 3600.0 / 8.0;

impl Default for Settings {
    fn default() -> Self {
        Self {
            sidetone: 0,
            lights: 5,
            inactive: 0,
            noise_control: 2,
            bands: [0.0; 10],
            eq_name: "flat".into(),
            eq_bypass: false,
            eq_user_presets: Vec::new(),
            mic: MicSettings::default(),
            chatmix: ChatMixSettings::default(),
            low_battery_percent: 25,
            notify_spare_ready: true,
            secs_per_step: NOMINAL_SECS_PER_STEP,
        }
    }
}

fn path() -> PathBuf {
    audio::app_config_dir().join("settings.json")
}

fn autostart_file() -> PathBuf {
    audio::config_home()
        .join("autostart")
        .join("arctis-control.desktop")
}

pub fn autostart_enabled() -> bool {
    autostart_file().exists()
}

pub fn set_autostart(enabled: bool) -> Result<bool, String> {
    let file = autostart_file();
    if !enabled {
        if file.exists() {
            std::fs::remove_file(&file).map_err(|e| format!("remove {}: {e}", file.display()))?;
        }
        return Ok(false);
    }
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let body = format!(
        "[Desktop Entry]\nType=Application\nName=Arctis Control\nComment=SteelSeries Arctis control panel\nExec=\"{}\" --hidden\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    audio::write_if_changed(&file, &body)?;
    Ok(true)
}

pub fn load() -> Settings {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) {
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        if let Err(e) = audio::write_if_changed(&path(), &json) {
            eprintln!("saving settings: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_file_still_loads() {
        let old = r#"{ "sidetone": 2, "lights": 7, "inactive": 3, "eq_preset": 0,
                       "bands": [1,0,0,0,0,0,0,0,0,-1], "mic_strength": 50 }"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.sidetone, 2);
        assert_eq!(s.bands[0], 1.0);
        assert_eq!(s.mic.preset, "radio");
        assert!(!s.chatmix.enabled);
    }
}
