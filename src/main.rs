#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod arctis;
mod audio;
mod chatmix;
mod engine;
mod mic;
mod store;
mod tray;

use arctis::Arctis;
use engine::{Engine, Snapshot};
use mic::MicSettings;
use std::sync::{Arc, Mutex};
use store::{EqPreset, Settings};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

pub struct Inner {
    pub dev: Option<Arctis>,
    pub settings: Settings,
}

#[derive(Default)]
struct Procs {
    meters: Vec<audio::Proc>,
    monitor: Option<audio::Proc>,
    player: Option<audio::Proc>,
}

pub struct App {
    inner: Arc<Mutex<Inner>>,
    engine: Arc<Engine>,
    procs: Arc<Mutex<Procs>>,
}

impl App {
    fn new(settings: Settings) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                dev: None,
                settings,
            })),
            engine: Arc::new(Engine::default()),
            procs: Arc::new(Mutex::new(Procs::default())),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, String> {
        self.inner.lock().map_err(|_| "state lock poisoned".to_string())
    }

    fn settings(&self) -> Settings {
        self.lock().map(|g| g.settings.clone()).unwrap_or_default()
    }

    /// Runs `f` against the command handle, reopening it once if it went stale
    /// (base station replugged).
    fn device<T>(&self, f: impl Fn(&mut Arctis) -> Result<T, String>) -> Result<T, String> {
        let mut inner = self.lock()?;
        let mut last_err = String::new();
        for _ in 0..2 {
            if inner.dev.is_none() {
                inner.dev = Some(Arctis::open()?);
            }
            let dev = inner.dev.as_mut().expect("device just opened");
            match f(dev) {
                Ok(v) => {
                    self.engine.poke();
                    return Ok(v);
                }
                Err(e) => {
                    inner.dev = None;
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    fn update(&self, f: impl FnOnce(&mut Settings)) {
        if let Ok(mut inner) = self.lock() {
            f(&mut inner.settings);
            store::save(&inner.settings);
        }
    }

    /// The state is reference-counted inside; commands clone the handles they
    /// need into blocking tasks.
    fn handles(&self) -> App {
        App {
            inner: self.inner.clone(),
            engine: self.engine.clone(),
            procs: self.procs.clone(),
        }
    }
}

// ----------------------------------------------------- shared actions ---
// Used by both the window (commands) and the tray menu.

fn do_noise_control(app: &App, mode: u8) -> Result<(), String> {
    app.device(|d| d.set_noise_control(mode))?;
    app.update(|s| s.noise_control = mode.min(2));
    Ok(())
}

fn do_mic(app: &App, on: bool) -> Result<mic::MicStatus, String> {
    let mut settings = app.settings().mic;
    settings.enabled = on;
    let status = if on {
        mic::enable(&settings)?
    } else {
        mic::disable(&settings)?
    };
    app.update(|s| s.mic.enabled = on);
    Ok(status)
}

fn do_chatmix(app: &App, on: bool) -> Result<chatmix::ChatMixStatus, String> {
    let mut settings = app.settings().chatmix;
    settings.enabled = on;
    let status = if on {
        chatmix::enable(&settings)?
    } else {
        chatmix::disable(&settings)?
    };
    let _ = app.device(|d| d.set_chatmix_reporting(on && settings.use_dial));
    app.update(|s| s.chatmix.enabled = on);
    Ok(status)
}

pub fn tray_action(handle: &AppHandle, id: &str) {
    let app = handle.state::<App>();
    let result = match id {
        "mic" => do_mic(&app, !app.settings().mic.enabled).map(|_| ()),
        "chatmix" => do_chatmix(&app, !app.settings().chatmix.enabled).map(|_| ()),
        _ => match id.strip_prefix("nc:").and_then(|m| m.parse().ok()) {
            Some(mode) => do_noise_control(&app, mode),
            None => Ok(()),
        },
    };
    if let Err(e) = result {
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "Arctis Control", "Arctis Control", &e])
            .status();
    }
    let s = app.settings();
    tray::set_feature_checks(handle, s.mic.enabled, s.chatmix.enabled);
    let _ = handle.emit("settings", &s);
}

pub fn quit(handle: &AppHandle) {
    let app = handle.state::<App>();
    if let Ok(mut procs) = app.procs.lock() {
        *procs = Procs::default();
    }
    // Hand the dial back to the base station.
    let _ = app.device(|d| d.set_chatmix_reporting(false));
    handle.exit(0);
}

// ---------------------------------------------------------- commands ---

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("task failed: {e}"))?
}

#[derive(serde::Serialize)]
struct Status {
    snapshot: Snapshot,
    settings: Settings,
    autostart: bool,
    mic_presets: Vec<MicSettings>,
}

#[tauri::command]
fn status(app: State<'_, App>) -> Status {
    Status {
        snapshot: app.engine.snapshot(),
        settings: app.settings(),
        autostart: store::autostart_enabled(),
        mic_presets: mic::PRESETS.iter().filter_map(|p| mic::preset(p)).collect(),
    }
}

#[tauri::command]
fn refresh(app: State<'_, App>) {
    app.engine.poke();
}

#[tauri::command]
async fn set_sidetone(app: State<'_, App>, level: u8) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        app.device(|d| d.set_sidetone(level))?;
        app.update(|s| s.sidetone = level.min(3));
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_lights(app: State<'_, App>, level: u8) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        app.device(|d| d.set_lights(level))?;
        app.update(|s| s.lights = level.clamp(1, 10));
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_auto_off(app: State<'_, App>, level: u8) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        app.device(|d| d.set_auto_off(level))?;
        app.update(|s| s.inactive = level.min(6));
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_noise_control(app: State<'_, App>, mode: u8) -> Result<(), String> {
    let app = app.handles();
    blocking(move || do_noise_control(&app, mode)).await
}

#[tauri::command]
async fn set_mic_volume(app: State<'_, App>, level: u8) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        app.device(|d| d.set_mic_volume(level))?;
        app.update(|s| s.mic.hw_volume = level.clamp(1, 10));
        Ok(())
    })
    .await
}

fn bands_from(bands: Vec<f32>) -> Result<[f32; 10], String> {
    let bands: [f32; 10] = bands
        .try_into()
        .map_err(|_| "equalizer needs exactly 10 bands".to_string())?;
    Ok(bands.map(|b| {
        if b.is_finite() {
            ((b * 2.0).round() / 2.0).clamp(-10.0, 10.0)
        } else {
            0.0
        }
    }))
}

#[tauri::command]
async fn set_equalizer(app: State<'_, App>, bands: Vec<f32>, name: String) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        let bands = bands_from(bands)?;
        if !app.settings().eq_bypass {
            app.device(|d| d.set_equalizer(&bands))?;
        }
        app.update(|s| {
            s.bands = bands;
            s.eq_name = name;
        });
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_eq_bypass(app: State<'_, App>, bypass: bool) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        let bands = if bypass { [0.0; 10] } else { app.settings().bands };
        app.device(|d| d.set_equalizer(&bands))?;
        app.update(|s| s.eq_bypass = bypass);
        Ok(())
    })
    .await
}

#[tauri::command]
fn save_eq_preset(
    app: State<'_, App>,
    name: String,
    bands: Vec<f32>,
) -> Result<Vec<EqPreset>, String> {
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 40 {
        return Err("preset names need 1-40 characters".into());
    }
    let bands = bands_from(bands)?;
    let mut presets = Vec::new();
    app.update(|s| {
        s.eq_user_presets.retain(|p| p.name != name);
        s.eq_user_presets.push(EqPreset {
            name: name.clone(),
            bands,
        });
        s.eq_name = format!("user:{name}");
        presets = s.eq_user_presets.clone();
    });
    Ok(presets)
}

#[tauri::command]
fn delete_eq_preset(app: State<'_, App>, name: String) -> Vec<EqPreset> {
    let mut presets = Vec::new();
    app.update(|s| {
        s.eq_user_presets.retain(|p| p.name != name);
        if s.eq_name == format!("user:{name}") {
            s.eq_name.clear();
        }
        presets = s.eq_user_presets.clone();
    });
    presets
}

#[tauri::command]
async fn mic_status(app: State<'_, App>) -> Result<mic::MicStatus, String> {
    let settings = app.settings().mic;
    blocking(move || Ok(mic::status(&settings))).await
}

#[tauri::command]
async fn mic_toggle(
    handle: AppHandle,
    app: State<'_, App>,
    on: bool,
) -> Result<mic::MicStatus, String> {
    let app = app.handles();
    blocking(move || {
        let result = do_mic(&app, on);
        let s = app.settings();
        tray::set_feature_checks(&handle, s.mic.enabled, s.chatmix.enabled);
        result
    })
    .await
}

/// Saves the full mic settings and pushes them live to the running chain.
#[tauri::command]
async fn mic_update(app: State<'_, App>, settings: MicSettings) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        let prev = app.settings().mic;
        let next = MicSettings {
            enabled: prev.enabled,
            ..settings
        };
        app.update(|s| s.mic = next.clone());
        if prev.enabled && audio::source_exists(mic::SOURCE_NAME) {
            mic::update(&prev, &next)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn mic_set_default() -> Result<(), String> {
    blocking(mic::set_default).await
}

#[tauri::command]
async fn mic_meter(handle: AppHandle, app: State<'_, App>, on: bool) -> Result<(), String> {
    let procs = app.procs.clone();
    blocking(move || {
        let mut procs = procs.lock().map_err(|_| "lock poisoned")?;
        procs.meters.clear();
        if !on {
            return Ok(());
        }
        let mut targets = Vec::new();
        if let Some(hw) = audio::hardware_mic() {
            targets.push(("raw", hw));
        }
        if audio::source_exists(mic::SOURCE_NAME) {
            targets.push(("studio", mic::SOURCE_NAME.to_string()));
        }
        for (which, target) in targets {
            let h = handle.clone();
            let meter = audio::spawn_meter(&target, move |level| {
                let _ = h.emit(
                    "meter",
                    serde_json::json!({ "which": which, "peak": level.peak_db, "rms": level.rms_db }),
                );
            })?;
            procs.meters.push(meter);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn mic_monitor(app: State<'_, App>, on: bool) -> Result<bool, String> {
    let procs = app.procs.clone();
    blocking(move || {
        let mut procs = procs.lock().map_err(|_| "lock poisoned")?;
        procs.monitor = None;
        if on {
            procs.monitor = Some(mic::spawn_monitor()?);
        }
        Ok(on)
    })
    .await
}

#[tauri::command]
async fn mic_test_record(seconds: u32) -> Result<bool, String> {
    blocking(move || mic::record_test(seconds)).await
}

#[tauri::command]
async fn mic_test_play(app: State<'_, App>, which: String) -> Result<(), String> {
    let procs = app.procs.clone();
    blocking(move || {
        let mut procs = procs.lock().map_err(|_| "lock poisoned")?;
        procs.player = None;
        if which != "stop" {
            procs.player = Some(mic::play_test(&which)?);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn chatmix_status(app: State<'_, App>) -> Result<chatmix::ChatMixStatus, String> {
    let settings = app.settings().chatmix;
    blocking(move || Ok(chatmix::status(&settings))).await
}

#[tauri::command]
async fn chatmix_toggle(
    handle: AppHandle,
    app: State<'_, App>,
    on: bool,
) -> Result<chatmix::ChatMixStatus, String> {
    let app = app.handles();
    blocking(move || {
        let result = do_chatmix(&app, on);
        let s = app.settings();
        tray::set_feature_checks(&handle, s.mic.enabled, s.chatmix.enabled);
        result
    })
    .await
}

#[tauri::command]
async fn chatmix_set_balance(app: State<'_, App>, balance: i8) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        let balance = balance.clamp(-100, 100);
        app.update(|s| s.chatmix.balance = balance);
        if app.settings().chatmix.enabled {
            chatmix::apply_balance(balance)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn chatmix_set_dial(app: State<'_, App>, on: bool) -> Result<(), String> {
    let app = app.handles();
    blocking(move || {
        app.update(|s| s.chatmix.use_dial = on);
        if app.settings().chatmix.enabled {
            app.device(|d| d.set_chatmix_reporting(on))?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn chatmix_set_default(which: String) -> Result<(), String> {
    blocking(move || chatmix::set_default(&which)).await
}

#[tauri::command]
fn set_notifications(app: State<'_, App>, low_percent: u8, spare_ready: bool) {
    app.update(|s| {
        s.low_battery_percent = low_percent.clamp(5, 50);
        s.notify_spare_ready = spare_ready;
    });
}

#[tauri::command]
fn set_autostart_enabled(enabled: bool) -> Result<bool, String> {
    store::set_autostart(enabled)
}

// --------------------------------------------------------------- CLI ---

fn print_json<T: serde::Serialize>(result: Result<T, String>) {
    match result {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).expect("serialize")),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

const HELP: &str = "Arctis Control — SteelSeries Arctis Nova Pro Wireless panel

usage: arctis-control [option]

  (none)                 open the window (and tray icon)
  --hidden               start in the tray only
  --page <name>          open on overview, eq, mic, chatmix, headset or settings
  --dump                 print the device status as JSON
  --anc off|transparency|on
  --mic-on [preset]      start the studio mic (natural, broadcast, podcast,
                         streaming, gaming)
  --mic-off              stop the studio mic
  --mic-status           print the studio mic status
  --chatmix on|off|<balance -100..100>
                         Game/Chat outputs; balance -100 = all game
  --autostart [on|off]   start at login
  --monitor              print raw vendor HID frames
  --help";

fn main() {
    // WebKitGTK renders a blank window on NVIDIA/XWayland when its DMA-BUF
    // renderer cannot allocate a GBM buffer.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(1) {
        Some("--help") | Some("-h") => println!("{HELP}"),
        Some("--dump") => print_json(Arctis::open().and_then(|mut d| d.status())),
        Some("--monitor") => monitor(),
        Some("--anc") => {
            let mode = match arg(2) {
                Some("off") => 0,
                Some("transparency") => 1,
                Some("on") | Some("anc") => 2,
                _ => {
                    eprintln!("--anc off|transparency|on");
                    std::process::exit(2);
                }
            };
            print_json(Arctis::open().and_then(|mut d| d.set_noise_control(mode)));
        }
        Some("--autostart") => match arg(2) {
            Some("on") => print_json(store::set_autostart(true)),
            Some("off") => print_json(store::set_autostart(false)),
            _ => println!("{}", store::autostart_enabled()),
        },
        Some("--mic-on") => {
            let mut settings = store::load();
            if let Some(p) = arg(2).and_then(mic::preset) {
                settings.mic = MicSettings {
                    hw_volume: settings.mic.hw_volume,
                    ..p
                };
            }
            settings.mic.enabled = true;
            let result = mic::enable(&settings.mic);
            if result.is_ok() {
                store::save(&settings);
            }
            print_json(result);
        }
        Some("--mic-off") => {
            let mut settings = store::load();
            settings.mic.enabled = false;
            let result = mic::disable(&settings.mic);
            store::save(&settings);
            print_json(result);
        }
        Some("--mic-status") => print_json(Ok(mic::status(&store::load().mic))),
        Some("--chatmix") => {
            let mut settings = store::load();
            let result = match arg(2) {
                Some("on") => {
                    settings.chatmix.enabled = true;
                    chatmix::enable(&settings.chatmix)
                }
                Some("off") => {
                    settings.chatmix.enabled = false;
                    chatmix::disable(&settings.chatmix)
                }
                Some(n) => match n.parse::<i8>() {
                    Ok(b) => {
                        settings.chatmix.balance = b.clamp(-100, 100);
                        chatmix::apply_balance(settings.chatmix.balance)
                            .map(|_| chatmix::status(&settings.chatmix))
                    }
                    Err(_) => Err("--chatmix on|off|<-100..100>".into()),
                },
                None => Ok(chatmix::status(&settings.chatmix)),
            };
            if result.is_ok() {
                store::save(&settings);
            }
            print_json(result);
        }
        Some("--hidden") | Some("--page") | None => run_app(),
        Some(other) => {
            eprintln!("unknown option {other}\n\n{HELP}");
            std::process::exit(2);
        }
    }
}

const PAGES: [&str; 6] = ["overview", "eq", "mic", "chatmix", "headset", "settings"];

fn run_app() {
    let mut settings = store::load();
    // The first version kept the mic chain inside PipeWire's own config; move
    // it to the separate service the first time this version starts.
    // An installed service is authoritative: an older build may have rewritten
    // settings.json without these fields.
    let mic_on = mic::legacy_enabled() || mic::SERVICE.is_installed();
    let chatmix_on = chatmix::SERVICE.is_installed();
    if (mic_on && !settings.mic.enabled) || (chatmix_on && !settings.chatmix.enabled) {
        settings.mic.enabled |= mic_on;
        settings.chatmix.enabled |= chatmix_on;
        store::save(&settings);
    }
    let app_state = App::new(settings.clone());
    let inner = app_state.inner.clone();
    let engine = app_state.engine.clone();

    tauri::Builder::default()
        .manage(app_state)
        .setup(move |app| {
            let handle = app.handle().clone();
            if let Err(e) = tray::setup(&handle, settings.mic.enabled, settings.chatmix.enabled) {
                eprintln!("system tray unavailable: {e}");
            }
            engine::spawn(handle.clone(), engine.clone(), inner.clone());

            // Bring the audio services up to date in the background (rebuilt
            // DSP plugin, moved headset, legacy config).
            let mic_settings = settings.mic.clone();
            let chatmix_settings = settings.chatmix.clone();
            std::thread::spawn(move || {
                if let Err(e) = mic::ensure(&mic_settings) {
                    eprintln!("studio mic: {e}");
                }
                if let Err(e) = chatmix::ensure(&chatmix_settings) {
                    eprintln!("chatmix: {e}");
                }
            });

            let page = std::env::args()
                .skip_while(|a| a != "--page")
                .nth(1)
                .filter(|p| PAGES.contains(&p.as_str()));
            if let (Some(page), Some(window)) = (page, handle.get_webview_window("main")) {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    let _ = window.eval(format!("showPage('{page}')"));
                });
            }

            if std::env::args().any(|a| a == "--hidden") {
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the tray running; Quit lives in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                if let Some(app) = window.app_handle().try_state::<App>() {
                    if let Ok(mut procs) = app.procs.lock() {
                        procs.meters.clear();
                        procs.monitor = None;
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            status,
            refresh,
            set_sidetone,
            set_lights,
            set_auto_off,
            set_noise_control,
            set_mic_volume,
            set_equalizer,
            set_eq_bypass,
            save_eq_preset,
            delete_eq_preset,
            mic_status,
            mic_toggle,
            mic_update,
            mic_set_default,
            mic_meter,
            mic_monitor,
            mic_test_record,
            mic_test_play,
            chatmix_status,
            chatmix_toggle,
            chatmix_set_balance,
            chatmix_set_dial,
            chatmix_set_default,
            set_notifications,
            set_autostart_enabled
        ])
        .run(tauri::generate_context!())
        .expect("error while running Arctis Control");
}

/// Raw vendor report dump, for discovering what the dial and buttons send.
fn monitor() {
    let mut dev = match Arctis::open() {
        Ok(dev) => dev,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "listening on {} — turn the ChatMix dial / press headset buttons; Ctrl-C to stop",
        dev.node.display()
    );
    loop {
        match dev.drain() {
            Ok(frames) => {
                for frame in frames {
                    let hex: Vec<String> =
                        frame.iter().take(24).map(|b| format!("{b:02x}")).collect();
                    println!("{}", hex.join(" "));
                }
            }
            Err(e) => {
                eprintln!("{e}");
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
