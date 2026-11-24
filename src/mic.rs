//! "Studio" microphone chain.
//!
//! The headset has no mic DSP command over HID, so processing happens in a
//! PipeWire filter-chain that captures the hardware mic and republishes it as
//! the virtual source `arctis_mic_enhanced` ("Arctis Mic (Studio)"):
//!
//! ```text
//! hw mic ─ high-pass ─┬─ DeepFilterNet | RNNoise ─┐
//!                     └──────── dry ──────────────┴─ mix ─ gate
//!   ─ breath reducer ─ leveler (AGC) ─ compressor      (dynamics)
//!   ─ warmth ─ depth ─ mud ─ clarity ─ air             (tone EQ)
//!   ─ tube/body ─ de-esser ─ limiter ─> source
//! ```
//!
//! The "broadcast" sound comes from the back half: a slow RMS leveler rides
//! the overall level like an engineer's hand on the fader, a faster
//! compressor adds density, the tube stage adds harmonics, and the body stage
//! synthesises the low harmonics a small headset capsule cannot pick up.
//!
//! Every stage is in the graph all the time and is neutral at its "off"
//! setting, so presets and sliders are applied live with `pw-cli set-param`;
//! only enabling/disabling or a missing plugin changes the process. The chain
//! runs in its own `pipewire` process (see `audio.rs`), so turning it on or off
//! never interrupts other audio.

use crate::audio::{self, Service};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

pub const SOURCE_NAME: &str = "arctis_mic_enhanced";
pub const CAPTURE_NODE: &str = "capture.arctis_mic";
const SOURCE_LABEL: &str = "Arctis Mic (Studio)";
const DSP_LIB: &str = "libarctis_dsp.so";

pub const SERVICE: Service = Service {
    unit: "arctis-mic.service",
    description: "Arctis Control studio microphone chain",
    conf_name: "mic-chain.conf",
};

/// The first version of this app wrote into PipeWire's own config directory.
fn legacy_config() -> PathBuf {
    audio::config_home().join("pipewire/pipewire.conf.d/90-arctis-mic.conf")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MicSettings {
    pub enabled: bool,
    pub preset: String,
    /// Hardware mic gain sent to the headset, 1-10.
    pub hw_volume: u8,

    pub highpass_hz: f32,
    pub noise_suppression: bool,
    /// "deepfilter" (DeepFilterNet) or "rnnoise".
    pub ns_engine: String,
    /// 0-100: how hard RNNoise's voice-activity gate closes between words.
    pub ns_strength: u8,
    pub gate: bool,
    pub gate_threshold_db: f32,
    /// Breath reduction depth in dB, 0 = off.
    pub breath_db: f32,

    pub warmth_db: f32,
    /// Chest resonance around 110 Hz.
    pub depth_db: f32,
    pub mud_db: f32,
    pub clarity_db: f32,
    pub air_db: f32,

    pub comp_threshold_db: f32,
    pub comp_ratio: f32,
    pub comp_attack_ms: f32,
    pub comp_release_ms: f32,
    pub comp_makeup_db: f32,
    /// Slow RMS leveler amount, 0-100.
    pub leveler: f32,
    /// Tube saturation amount, 0-100.
    pub tube: f32,
    /// Synthesised low harmonics, 0-100.
    pub body: f32,

    pub deess_db: f32,
    pub deess_hz: f32,

    pub output_db: f32,
    pub ceiling_db: f32,
}

impl Default for MicSettings {
    fn default() -> Self {
        preset("radio").expect("builtin preset")
    }
}

pub const PRESETS: [&str; 6] = ["radio", "broadcast", "podcast", "natural", "streaming", "gaming"];

/// Built-in voice presets. All of them keep the limiter at -1 dBFS.
pub fn preset(name: &str) -> Option<MicSettings> {
    let base = MicSettings {
        enabled: false,
        preset: name.to_string(),
        hw_volume: 10,
        highpass_hz: 80.0,
        noise_suppression: true,
        ns_engine: "deepfilter".into(),
        ns_strength: 35,
        gate: false,
        gate_threshold_db: -55.0,
        breath_db: 8.0,
        warmth_db: 0.0,
        depth_db: 0.0,
        mud_db: 0.0,
        clarity_db: 0.0,
        air_db: 0.0,
        comp_threshold_db: -20.0,
        comp_ratio: 2.0,
        comp_attack_ms: 8.0,
        comp_release_ms: 150.0,
        comp_makeup_db: 3.0,
        leveler: 0.0,
        tube: 0.0,
        body: 0.0,
        deess_db: 3.0,
        deess_hz: 6500.0,
        output_db: 0.0,
        ceiling_db: -1.0,
    };
    let s = match name {
        // Deep, dense FM-radio voice: chest resonance, synthesised low
        // harmonics, tube colour and two-stage compression.
        "radio" => MicSettings {
            highpass_hz: 70.0,
            ns_strength: 55,
            gate: true,
            gate_threshold_db: -50.0,
            breath_db: 15.0,
            warmth_db: 3.0,
            depth_db: 7.0,
            mud_db: -1.0,
            clarity_db: 2.0,
            air_db: 2.0,
            comp_threshold_db: -28.0,
            comp_ratio: 6.0,
            comp_attack_ms: 0.8,
            comp_release_ms: 110.0,
            comp_makeup_db: 8.5,
            leveler: 80.0,
            tube: 45.0,
            body: 50.0,
            deess_db: 6.0,
            output_db: 4.0,
            ..base
        },
        // Clean, lightly levelled — sounds like the mic, minus the room.
        "natural" => MicSettings {
            clarity_db: 1.5,
            leveler: 20.0,
            ..base
        },
        // Close, warm "radio" voice with dense levelling.
        "broadcast" => MicSettings {
            highpass_hz: 90.0,
            ns_strength: 45,
            gate: true,
            gate_threshold_db: -52.0,
            warmth_db: 3.0,
            depth_db: 2.5,
            mud_db: -3.0,
            clarity_db: 3.0,
            air_db: 2.5,
            leveler: 50.0,
            tube: 30.0,
            body: 30.0,
            comp_threshold_db: -26.0,
            comp_ratio: 4.0,
            comp_attack_ms: 4.0,
            comp_release_ms: 160.0,
            comp_makeup_db: 9.0,
            deess_db: 6.0,
            ..base
        },
        "podcast" => MicSettings {
            ns_strength: 40,
            gate: true,
            gate_threshold_db: -58.0,
            warmth_db: 2.0,
            depth_db: 1.5,
            mud_db: -2.5,
            clarity_db: 2.0,
            air_db: 1.5,
            leveler: 45.0,
            tube: 15.0,
            body: 15.0,
            comp_threshold_db: -22.0,
            comp_ratio: 3.0,
            comp_attack_ms: 6.0,
            comp_release_ms: 180.0,
            comp_makeup_db: 6.0,
            deess_db: 5.0,
            ..base
        },
        // Bright and forward so it cuts through game audio on stream.
        "streaming" => MicSettings {
            highpass_hz: 100.0,
            ns_strength: 50,
            gate: true,
            gate_threshold_db: -50.0,
            warmth_db: 1.0,
            mud_db: -3.5,
            clarity_db: 4.0,
            air_db: 3.0,
            leveler: 40.0,
            tube: 15.0,
            comp_threshold_db: -22.0,
            comp_ratio: 4.0,
            comp_attack_ms: 3.0,
            comp_release_ms: 120.0,
            comp_makeup_db: 8.0,
            deess_db: 6.0,
            deess_hz: 7000.0,
            ..base
        },
        // Maximum background removal for noisy rooms and keyboards.
        "gaming" => MicSettings {
            highpass_hz: 120.0,
            ns_strength: 85,
            gate: true,
            gate_threshold_db: -45.0,
            mud_db: -2.0,
            clarity_db: 3.5,
            air_db: 1.0,
            comp_threshold_db: -20.0,
            comp_ratio: 3.0,
            comp_attack_ms: 5.0,
            comp_makeup_db: 6.0,
            deess_db: 4.0,
            ..base
        },
        _ => return None,
    };
    Some(s)
}

#[derive(Debug, Serialize, Clone)]
pub struct MicStatus {
    pub enabled: bool,
    pub active: bool,
    pub is_default: bool,
    pub rnnoise: bool,
    pub deepfilter: bool,
    /// Engine the running chain uses ("deepfilter", "rnnoise" or "none").
    pub engine: &'static str,
    pub dsp: bool,
    pub hardware: Option<String>,
    pub source: &'static str,
}

pub fn deepfilter_path() -> Option<PathBuf> {
    [
        "/usr/lib/ladspa/libdeep_filter_ladspa.so",
        "/usr/lib64/ladspa/libdeep_filter_ladspa.so",
        "/usr/local/lib/ladspa/libdeep_filter_ladspa.so",
        "/usr/lib/x86_64-linux-gnu/ladspa/libdeep_filter_ladspa.so",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// Noise engine actually used: the preferred one if installed, else the
/// other one, else none.
pub fn engine(s: &MicSettings) -> &'static str {
    let dfn = deepfilter_path().is_some();
    let rnn = rnnoise_path().is_some();
    match (s.ns_engine.as_str(), dfn, rnn) {
        ("rnnoise", _, true) => "rnnoise",
        (_, true, _) => "deepfilter",
        (_, false, true) => "rnnoise",
        _ => "none",
    }
}

pub fn rnnoise_path() -> Option<PathBuf> {
    [
        "/usr/lib/ladspa/librnnoise_ladspa.so",
        "/usr/lib64/ladspa/librnnoise_ladspa.so",
        "/usr/local/lib/ladspa/librnnoise_ladspa.so",
        "/usr/lib/x86_64-linux-gnu/ladspa/librnnoise_ladspa.so",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// Where the running chain loads our LADSPA plugin from. A stable copy, so a
/// `cargo clean` does not pull the library out from under PipeWire.
fn installed_dsp() -> PathBuf {
    audio::data_home().join("arctis-control").join(DSP_LIB)
}

/// The freshly built plugin shipped next to the executable.
fn bundled_dsp() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join(DSP_LIB),
        dir.join("../lib/arctis-control").join(DSP_LIB),
        PathBuf::from("/usr/lib/arctis-control").join(DSP_LIB),
        PathBuf::from("/usr/local/lib/arctis-control").join(DSP_LIB),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// Copies the bundled plugin into place if it differs. Returns the path to
/// load and whether an existing copy was replaced (a running chain then needs
/// a restart to pick it up).
fn install_dsp() -> (Option<PathBuf>, bool) {
    let target = installed_dsp();
    let Some(src) = bundled_dsp() else {
        return (target.exists().then_some(target), false);
    };
    let fresh = std::fs::read(&src).ok();
    let current = std::fs::read(&target).ok();
    if fresh.is_some() && fresh != current {
        if let Some(dir) = target.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = target.with_extension("so.tmp");
        let ok = std::fs::write(&tmp, fresh.unwrap_or_default()).is_ok()
            && std::fs::rename(&tmp, &target).is_ok();
        return (target.exists().then_some(target), ok && current.is_some());
    }
    (target.exists().then_some(target), false)
}

pub fn status(settings: &MicSettings) -> MicStatus {
    MicStatus {
        enabled: settings.enabled,
        active: audio::source_exists(SOURCE_NAME),
        is_default: audio::default_source().as_deref() == Some(SOURCE_NAME),
        rnnoise: rnnoise_path().is_some(),
        deepfilter: deepfilter_path().is_some(),
        engine: engine(settings),
        dsp: installed_dsp().exists() || bundled_dsp().is_some(),
        hardware: audio::hardware_mic(),
        source: SOURCE_NAME,
    }
}

/// Starts (or reconfigures) the chain and pushes all parameters live.
pub fn enable(settings: &MicSettings) -> Result<MicStatus, String> {
    remove_legacy()?;
    let (dsp, dsp_updated) = install_dsp();
    let hardware = audio::hardware_mic();
    // PipeWire's software volume on the raw mic sits in front of the chain;
    // keep it at unity so the hardware gain and the leveler do the work.
    if let Some(hw) = &hardware {
        let _ = audio::run("pactl", &["set-source-volume", hw, "100%"]);
    }
    let conf = render(settings, engine(settings), dsp.as_deref(), hardware.as_deref());
    SERVICE.apply(&conf)?;
    if dsp_updated {
        SERVICE.restart()?;
    }
    if !audio::wait_for(Duration::from_secs(6), || audio::source_exists(SOURCE_NAME)) {
        return Err(
            "the studio microphone did not appear — see `journalctl --user -u arctis-mic`".into(),
        );
    }
    // The config already carries these values; pushing them again covers a
    // chain that was running with older ones.
    let _ = apply_live(settings);
    Ok(status(settings))
}

pub fn disable(settings: &MicSettings) -> Result<MicStatus, String> {
    // Hand the default input back to the hardware mic before it disappears.
    if audio::default_source().as_deref() == Some(SOURCE_NAME) {
        if let Some(hw) = audio::hardware_mic() {
            let _ = audio::set_default_source(&hw);
        }
    }
    SERVICE.remove()?;
    remove_legacy()?;
    Ok(status(settings))
}

/// Called at start-up: migrates the legacy drop-in and refreshes a running
/// chain whose plugin or graph is out of date.
pub fn ensure(settings: &MicSettings) -> Result<(), String> {
    if settings.enabled {
        enable(settings).map(|_| ())
    } else {
        remove_legacy()
    }
}

/// Whether the old in-PipeWire drop-in from the first version is present.
pub fn legacy_enabled() -> bool {
    legacy_config().exists()
}

fn remove_legacy() -> Result<(), String> {
    let legacy = legacy_config();
    if legacy.exists() {
        std::fs::remove_file(&legacy).map_err(|e| format!("remove {}: {e}", legacy.display()))?;
        audio::run("systemctl", &["--user", "restart", "pipewire"])?;
        std::thread::sleep(Duration::from_millis(800));
    }
    Ok(())
}

pub fn set_default() -> Result<(), String> {
    if !audio::source_exists(SOURCE_NAME) {
        return Err("the studio microphone is not running".into());
    }
    audio::set_default_source(SOURCE_NAME)
}

/// Applies new settings to a running chain: live when only values changed,
/// a quick restart of the mic service when the noise engine changed. The
/// config file is kept in sync either way, so the next start needs no
/// restart.
pub fn update(prev: &MicSettings, next: &MicSettings) -> Result<(), String> {
    if engine(prev) != engine(next) {
        return enable(next).map(|_| ());
    }
    let dsp = installed_dsp();
    let conf = render(
        next,
        engine(next),
        dsp.exists().then_some(dsp.as_path()),
        audio::hardware_mic().as_deref(),
    );
    let _ = audio::write_if_changed(&SERVICE.conf_path(), &conf);
    apply_live(next)
}

/// Pushes every control to the running graph — no restart, no dropout.
pub fn apply_live(settings: &MicSettings) -> Result<(), String> {
    let dsp = installed_dsp().exists();
    let params: Vec<(String, f32)> = controls(settings, engine(settings), dsp)
        .into_iter()
        .flat_map(|(node, ctl)| {
            ctl.into_iter()
                .map(move |(port, v)| (format!("{node}:{port}"), v))
        })
        .collect();
    audio::set_filter_params(CAPTURE_NODE, &params)
}

/// RNNoise VAD threshold for a 0-100 strength. 0 still denoises, it just
/// never mutes between words.
fn vad_threshold(strength: u8) -> f32 {
    f32::from(strength.min(100)) * 0.9
}

type Controls = Vec<(&'static str, Vec<(&'static str, f32)>)>;

/// Control values per graph node, for both the config file and live updates.
fn controls(s: &MicSettings, engine: &str, dsp: bool) -> Controls {
    let mut c: Controls = vec![("hp", vec![("Freq", s.highpass_hz.clamp(20.0, 300.0))])];
    let strength = f32::from(s.ns_strength.min(100));
    match engine {
        "rnnoise" => c.push((
            "rnnoise",
            vec![
                ("VAD Threshold (%)", vad_threshold(s.ns_strength)),
                ("VAD Grace Period (ms)", 200.0),
                ("Retroactive VAD Grace (ms)", 0.0),
            ],
        )),
        "deepfilter" => c.push((
            "dfn",
            vec![
                // How far noise may be pushed down: 12 dB (gentle) … 100 dB.
                ("Attenuation Limit (dB)", 12.0 + strength * 0.88),
                ("Min processing threshold (dB)", -15.0),
                ("Max ERB processing threshold (dB)", 35.0),
                ("Max DF processing threshold (dB)", 35.0),
                ("Min Processing Buffer (frames)", 0.0),
                ("Post Filter Beta", if strength >= 60.0 { 0.02 } else { 0.0 }),
            ],
        )),
        _ => {}
    }
    if engine != "none" {
        let ns = s.noise_suppression;
        c.push((
            "ns",
            vec![
                ("Gain 1", if ns { 1.0 } else { 0.0 }),
                ("Gain 2", if ns { 0.0 } else { 1.0 }),
            ],
        ));
    }
    if dsp {
        c.push((
            "gate",
            vec![
                ("Threshold (dB)", s.gate_threshold_db.clamp(-90.0, 0.0)),
                ("Range (dB)", if s.gate { -30.0 } else { 0.0 }),
                ("Attack (ms)", 1.0),
                ("Hold (ms)", 180.0),
                ("Release (ms)", 200.0),
            ],
        ));
    }
    let eq = |freq: f32, q: f32, gain: f32| vec![("Freq", freq), ("Q", q), ("Gain", gain.clamp(-12.0, 12.0))];
    // Voice fundamentals sit around 85-180 Hz and their first harmonics up
    // to ~350 Hz: that "chest" region is what reads as a deep voice.
    c.push(("warmth", eq(200.0, 0.7, s.warmth_db)));
    c.push(("depth", eq(150.0, 0.8, s.depth_db)));
    c.push(("mud", eq(500.0, 1.4, s.mud_db)));
    c.push(("clarity", eq(3800.0, 0.9, s.clarity_db)));
    c.push(("air", eq(10000.0, 0.7, s.air_db)));
    if dsp {
        c.push((
            "breath",
            vec![
                ("Amount (dB)", s.breath_db.clamp(0.0, 40.0)),
                ("Sensitivity", 0.5),
            ],
        ));
        c.push((
            "leveler",
            vec![
                ("Target (dB)", -18.0),
                ("Max Boost (dB)", 20.0),
                ("Max Cut (dB)", 12.0),
                // Never ride up anything quieter than the gate lets through.
                ("Floor (dB)", s.gate_threshold_db.clamp(-70.0, -35.0)),
                ("Amount", s.leveler.clamp(0.0, 100.0) / 100.0),
            ],
        ));
        c.push((
            "comp",
            vec![
                ("Threshold (dB)", s.comp_threshold_db.clamp(-60.0, 0.0)),
                ("Ratio", s.comp_ratio.clamp(1.0, 20.0)),
                ("Knee (dB)", 6.0),
                ("Attack (ms)", s.comp_attack_ms.clamp(0.1, 200.0)),
                ("Release (ms)", s.comp_release_ms.clamp(5.0, 2000.0)),
                ("Makeup (dB)", s.comp_makeup_db.clamp(0.0, 30.0)),
                // Near-peak detection: catches syllable onsets, which is what
                // brings the crest factor down to broadcast density.
                ("RMS (ms)", 1.0),
            ],
        ));
        let body = s.body.clamp(0.0, 100.0);
        c.push((
            "tube",
            vec![
                ("Drive (dB)", s.tube.clamp(0.0, 100.0) * 0.18),
                ("Body (dB)", if body <= 0.0 { -60.0 } else { -30.0 + body * 0.33 }),
                ("Body Freq (Hz)", 220.0),
            ],
        ));
        c.push((
            "deess",
            vec![
                ("Frequency (Hz)", s.deess_hz.clamp(2000.0, 16000.0)),
                ("Threshold (dB)", -24.0),
                ("Amount (dB)", s.deess_db.clamp(0.0, 24.0)),
            ],
        ));
        c.push((
            "limiter",
            vec![
                ("Input Gain (dB)", s.output_db.clamp(-24.0, 24.0)),
                ("Ceiling (dB)", s.ceiling_db.clamp(-24.0, 0.0)),
                ("Release (ms)", 80.0),
            ],
        ));
    }
    c
}

fn node_line(kind: &str, name: &str, plugin: &str, label: &str, ctl: &[(&str, f32)]) -> String {
    let controls: Vec<String> = ctl.iter().map(|(k, v)| format!("\"{k}\" = {v:.3}")).collect();
    let plugin = if plugin.is_empty() {
        String::new()
    } else {
        format!("plugin = \"{plugin}\" ")
    };
    format!(
        "          {{ type = {kind} name = {name} {plugin}label = {label} control = {{ {} }} }}",
        controls.join(" ")
    )
}

pub fn render(
    s: &MicSettings,
    engine: &str,
    dsp: Option<&std::path::Path>,
    hardware: Option<&str>,
) -> String {
    let ctl = controls(s, engine, dsp.is_some());
    let get = |name: &str| -> Vec<(&str, f32)> {
        ctl.iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| c.clone())
            .unwrap_or_default()
    };
    let dsp_path = dsp.map(|p| p.display().to_string()).unwrap_or_default();

    let mut nodes = vec![node_line("builtin", "hp", "", "bq_highpass", &get("hp"))];
    let mut links: Vec<(String, String)> = Vec::new();
    let mut tail = "hp:Out".to_string();

    let nr = match engine {
        "rnnoise" => Some(("rnnoise", "librnnoise_ladspa", "noise_suppressor_mono", "Input", "Output")),
        "deepfilter" => Some(("dfn", "libdeep_filter_ladspa", "deep_filter_mono", "Audio In", "Audio Out")),
        _ => None,
    };
    if let Some((name, plugin, label, input, output)) = nr {
        nodes.push(node_line("ladspa", name, plugin, label, &get(name)));
        nodes.push(node_line("builtin", "ns", "", "mixer", &get("ns")));
        links.push((tail.clone(), format!("{name}:{input}")));
        links.push((tail.clone(), "ns:In 2".into()));
        links.push((format!("{name}:{output}"), "ns:In 1".into()));
        tail = "ns:Out".into();
    }

    // Dynamics first, tone after: compressing *after* a bass boost would
    // turn the boosted vowels back down and undo the depth.
    let mut chain: Vec<(&str, &str, &str)> = Vec::new();
    if dsp.is_some() {
        chain.extend([
            ("gate", "ladspa", "arctis_gate"),
            // Before the leveler and compressor, which would lift breaths.
            ("breath", "ladspa", "arctis_breath"),
            ("leveler", "ladspa", "arctis_agc"),
            ("comp", "ladspa", "arctis_comp"),
        ]);
    }
    chain.extend([
        ("warmth", "builtin", "bq_lowshelf"),
        ("depth", "builtin", "bq_peaking"),
        ("mud", "builtin", "bq_peaking"),
        ("clarity", "builtin", "bq_peaking"),
        ("air", "builtin", "bq_highshelf"),
    ]);
    if dsp.is_some() {
        chain.extend([
            ("tube", "ladspa", "arctis_warmth"),
            ("deess", "ladspa", "arctis_deesser"),
            ("limiter", "ladspa", "arctis_limiter"),
        ]);
    }
    for (name, kind, label) in chain {
        let plugin = if kind == "ladspa" { dsp_path.as_str() } else { "" };
        nodes.push(node_line(kind, name, plugin, label, &get(name)));
        links.push((tail.clone(), format!("{name}:In")));
        tail = format!("{name}:Out");
    }

    let links: Vec<String> = links
        .iter()
        .map(|(o, i)| format!("          {{ output = \"{o}\" input = \"{i}\" }}"))
        .collect();
    let target = hardware
        .map(|h| format!("\n        target.object = \"{h}\""))
        .unwrap_or_default();

    format!(
        "# Generated by Arctis Control — change settings in the app, not here.\n\
context.properties = {{ log.level = 0 }}\n\
context.spa-libs = {{\n\
  audio.convert.* = audioconvert/libspa-audioconvert\n\
  support.*       = support/libspa-support\n\
}}\n\
context.modules = [\n\
  {{ name = libpipewire-module-rt flags = [ ifexists nofail ] }}\n\
  {{ name = libpipewire-module-protocol-native }}\n\
  {{ name = libpipewire-module-client-node }}\n\
  {{ name = libpipewire-module-adapter }}\n\
  {{ name = libpipewire-module-filter-chain\n\
    args = {{\n\
      node.description = \"{SOURCE_LABEL}\"\n\
      media.name = \"{SOURCE_LABEL}\"\n\
      filter.graph = {{\n\
        nodes = [\n{}\n        ]\n\
        links = [\n{}\n        ]\n\
      }}\n\
      capture.props = {{\n\
        node.name = \"{CAPTURE_NODE}\"\n\
        node.passive = true\n\
        node.dont-fallback = true\n\
        audio.rate = 48000\n\
        audio.position = [ MONO ]{target}\n\
      }}\n\
      playback.props = {{\n\
        node.name = \"{SOURCE_NAME}\"\n\
        node.description = \"{SOURCE_LABEL}\"\n\
        media.class = Audio/Source\n\
        audio.rate = 48000\n\
        audio.position = [ MONO ]\n\
      }}\n\
    }}\n\
  }}\n\
]\n",
        nodes.join("\n"),
        links.join("\n")
    )
}

// --------------------------------------------------------- mic test ---

/// Records the processed and the raw mic side by side, so the user can A/B
/// what the chain does to their own voice.
pub fn record_test(seconds: u32) -> Result<bool, String> {
    let hardware = audio::hardware_mic().ok_or("headset microphone not found")?;
    let dir = audio::runtime_dir();
    let secs = seconds.clamp(2, 15).to_string();
    let studio = audio::source_exists(SOURCE_NAME);
    let mut children = Vec::new();
    for (target, file) in [
        (SOURCE_NAME, "arctis-test-studio.wav"),
        (hardware.as_str(), "arctis-test-raw.wav"),
    ] {
        let path = dir.join(file);
        let _ = std::fs::remove_file(&path);
        if target == SOURCE_NAME && !studio {
            continue;
        }
        children.push(
            std::process::Command::new("timeout")
                .args([secs.as_str(), "pw-record", "--target", target])
                // Never fall back to another input if the target vanishes.
                .args(["-P", "{ node.dont-fallback = true node.dont-reconnect = true }"])
                .args(["--rate", "48000", "--channels", "1"])
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("pw-record: {e}"))?,
        );
    }
    for mut child in children {
        let _ = child.wait();
    }
    Ok(studio)
}

pub fn play_test(which: &str) -> Result<audio::Proc, String> {
    let file = if which == "raw" {
        "arctis-test-raw.wav"
    } else {
        "arctis-test-studio.wav"
    };
    let path = audio::runtime_dir().join(file);
    if !path.exists() {
        return Err("record a test first".into());
    }
    audio::spawn("pw-play", &[path.display().to_string()])
}

/// Live monitoring of the processed mic on the default output.
pub fn spawn_monitor() -> Result<audio::Proc, String> {
    if !audio::source_exists(SOURCE_NAME) {
        return Err("turn the studio microphone on first".into());
    }
    audio::spawn(
        "pw-loopback",
        &[
            "--capture-props".into(),
            format!(
                "{{ target.object = \"{SOURCE_NAME}\" node.name = arctis_mic_listen node.dont-fallback = true }}"
            ),
            "--playback-props".into(),
            "{ node.name = arctis_mic_listen.out media.role = Communication }".into(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_preset_exists_and_is_named() {
        for name in PRESETS {
            assert_eq!(preset(name).unwrap().preset, name);
        }
        assert!(preset("nope").is_none());
    }

    #[test]
    fn full_chain_links_in_order() {
        let conf = render(
            &MicSettings::default(),
            "deepfilter",
            Some(Path::new("/x/libarctis_dsp.so")),
            Some("alsa_input.hw"),
        );
        let order = [
            "hp:Out", "dfn:Audio Out", "ns:Out", "gate:Out", "breath:Out", "leveler:Out",
            "comp:Out",
            "warmth:Out", "depth:Out", "mud:Out", "clarity:Out", "air:Out", "tube:Out",
            "deess:Out",
        ];
        let mut last = 0;
        for o in order {
            let at = conf
                .find(&format!("output = \"{o}\""))
                .unwrap_or_else(|| panic!("{o} missing"));
            assert!(at >= last, "{o} out of order");
            last = at;
        }
        assert!(conf.contains("label = arctis_limiter"));
        assert!(conf.contains("plugin = \"/x/libarctis_dsp.so\""));
        assert!(conf.contains("target.object = \"alsa_input.hw\""));
        assert!(conf.contains("media.class = Audio/Source"));
    }

    #[test]
    fn degrades_without_plugins() {
        let conf = render(&MicSettings::default(), "none", None, None);
        assert!(!conf.contains("rnnoise") && !conf.contains("deep_filter"));
        assert!(!conf.contains("arctis_comp"));
        assert!(conf.contains("output = \"hp:Out\" input = \"warmth:In\""));
        assert!(!conf.contains("target.object"));
    }

    #[test]
    fn noise_suppression_switches_mixer_live() {
        let s = MicSettings {
            noise_suppression: false,
            ..Default::default()
        };
        let c = controls(&s, "rnnoise", true);
        let ns = &c.iter().find(|(n, _)| *n == "ns").unwrap().1;
        assert_eq!(ns, &vec![("Gain 1", 0.0), ("Gain 2", 1.0)]);
    }

    #[test]
    fn rnnoise_graph_uses_its_port_names() {
        let conf = render(&MicSettings::default(), "rnnoise", None, None);
        assert!(conf.contains("input = \"rnnoise:Input\""));
        assert!(conf.contains("output = \"rnnoise:Output\""));
        assert!(!conf.contains("deep_filter"));
    }

    #[test]
    fn neutral_character_stages_are_off() {
        let s = MicSettings { leveler: 0.0, tube: 0.0, body: 0.0, ..Default::default() };
        let c = controls(&s, "none", true);
        let get = |n: &str| c.iter().find(|(name, _)| *name == n).unwrap().1.clone();
        assert!(get("leveler").contains(&("Amount", 0.0)));
        assert!(get("tube").contains(&("Drive (dB)", 0.0)));
        assert!(get("tube").contains(&("Body (dB)", -60.0)));
    }

    #[test]
    fn gate_off_means_zero_range() {
        let s = MicSettings {
            gate: false,
            ..Default::default()
        };
        let c = controls(&s, "none", true);
        let gate = &c.iter().find(|(n, _)| *n == "gate").unwrap().1;
        assert!(gate.contains(&("Range (dB)", 0.0)));
    }
}
