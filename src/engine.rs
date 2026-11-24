//! Background device thread. Owns its own hidraw handle (every open handle
//! receives every input report), so it can react to the base station's
//! unsolicited events — ChatMix dial, ANC button, battery changes — within
//! ~50 ms while the command path keeps its own handle.
//!
//! It keeps the latest `Snapshot`, pushes it to the window as a `device`
//! event and to the tray, learns how fast the battery drains, and sends the
//! low-battery / spare-ready notifications.

use crate::arctis::{self, Arctis, DeviceStatus, Event};
use crate::{chatmix, store, tray, Inner};
use serde::Serialize;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const TICK: Duration = Duration::from_millis(50);
const POLL: Duration = Duration::from_secs(10);
const REOPEN: Duration = Duration::from_secs(2);
const LOW_COOLDOWN: Duration = Duration::from_secs(45 * 60);
const DIAL_INTERVAL: Duration = Duration::from_millis(60);

#[derive(Debug, Serialize, Clone, Default)]
pub struct Snapshot {
    pub connected: bool,
    pub node: Option<String>,
    pub device: Option<DeviceStatus>,
    pub error: Option<String>,
    /// Estimated time on the current battery, while it is discharging.
    pub minutes_left: Option<u32>,
    /// The estimate comes from a measured drain rate, not the rated one.
    pub estimate_learned: bool,
    pub updated: u64,
}

#[derive(Default)]
pub struct Engine {
    pub snapshot: Mutex<Snapshot>,
    poke: AtomicBool,
}

impl Engine {
    /// Ask for a status read on the next tick (after a write, say).
    pub fn poke(&self) {
        self.poke.store(true, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Learns seconds-per-gauge-step from whole steps observed while discharging.
#[derive(Default)]
struct BatteryClock {
    last_raw: Option<u8>,
    step_started: Option<Instant>,
}

impl BatteryClock {
    /// Returns a new measured duration of one step, if one just completed.
    fn observe(&mut self, raw: u8, discharging: bool) -> Option<f32> {
        let mut measured = None;
        if !discharging {
            self.step_started = None;
        } else if let Some(prev) = self.last_raw {
            if raw < prev {
                if let (Some(start), true) = (self.step_started, prev - raw == 1) {
                    measured = Some(start.elapsed().as_secs_f32());
                }
                self.step_started = Some(Instant::now());
            } else if raw > prev {
                self.step_started = None;
            }
        }
        self.last_raw = Some(raw);
        measured
    }

    fn minutes_left(&self, raw: u8, secs_per_step: f32) -> u32 {
        let into_step = self
            .step_started
            .map(|s| s.elapsed().as_secs_f32().min(secs_per_step * 0.95))
            .unwrap_or(secs_per_step * 0.5);
        let secs = (f32::from(raw) * secs_per_step - into_step).max(0.0);
        (secs / 60.0).round() as u32
    }
}

pub fn spawn(app: AppHandle, engine: Arc<Engine>, inner: Arc<Mutex<Inner>>) {
    std::thread::spawn(move || {
        let mut dev: Option<Arctis> = None;
        let mut next_open = Instant::now();
        let mut next_poll = Instant::now();
        let mut clock = BatteryClock::default();
        let mut last_low: Option<Instant> = None;
        let mut last: Option<DeviceStatus> = None;
        let mut pending_dial: Option<i8> = None;
        let mut last_dial = Instant::now() - DIAL_INTERVAL;

        loop {
            if dev.is_none() && Instant::now() >= next_open {
                next_open = Instant::now() + REOPEN;
                match Arctis::open() {
                    Ok(mut d) => {
                        let dial = with_settings(&inner, |s| s.chatmix.enabled && s.chatmix.use_dial);
                        if dial {
                            let _ = d.set_chatmix_reporting(true);
                        }
                        dev = Some(d);
                        next_poll = Instant::now();
                    }
                    Err(e) => publish_offline(&app, &engine, e),
                }
            }

            if let Some(d) = dev.as_mut() {
                match d.drain() {
                    Ok(frames) => {
                        for f in frames {
                            match arctis::parse_event(&f) {
                                Some(Event::ChatMix { game, chat }) => {
                                    pending_dial = Some(chatmix::balance_from(game, chat));
                                }
                                Some(Event::Changed) => {
                                    next_poll = next_poll.min(Instant::now() + Duration::from_millis(120));
                                }
                                None => {}
                            }
                        }
                    }
                    Err(e) => {
                        dev = None;
                        publish_offline(&app, &engine, e);
                        continue;
                    }
                }
            }

            if let Some(balance) = pending_dial {
                if last_dial.elapsed() >= DIAL_INTERVAL {
                    pending_dial = None;
                    last_dial = Instant::now();
                    apply_dial(&app, &inner, balance);
                }
            }

            if engine.poke.swap(false, Ordering::Relaxed) {
                next_poll = Instant::now();
            }

            if let Some(d) = dev.as_mut() {
                if Instant::now() >= next_poll {
                    next_poll = Instant::now() + POLL;
                    match d.status() {
                        Ok(status) => {
                            let node = Some(d.node.display().to_string());
                            on_status(&app, &engine, &inner, &mut clock, &mut last_low, last, status, node);
                            last = Some(status);
                        }
                        Err(e) => {
                            dev = None;
                            last = None;
                            publish_offline(&app, &engine, e);
                        }
                    }
                }
            }

            std::thread::sleep(TICK);
        }
    });
}

fn with_settings<T>(inner: &Arc<Mutex<Inner>>, f: impl FnOnce(&mut store::Settings) -> T) -> T
where
    T: Default,
{
    inner.lock().map(|mut g| f(&mut g.settings)).unwrap_or_default()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn on_status(
    app: &AppHandle,
    engine: &Engine,
    inner: &Arc<Mutex<Inner>>,
    clock: &mut BatteryClock,
    last_low: &mut Option<Instant>,
    previous: Option<DeviceStatus>,
    status: DeviceStatus,
    node: Option<String>,
) {
    let h = status.headset;
    let discharging = h.online && !h.charging;

    let (secs_per_step, low_at, notify_spare) = with_settings(inner, |s| {
        if let Some(sample) = clock.observe(h.raw, discharging) {
            // Ignore implausible samples (sleep/resume, headset swapped).
            if (600.0..=8.0 * 3600.0).contains(&sample) {
                s.secs_per_step = 0.7 * s.secs_per_step + 0.3 * sample;
                store::save(s);
            }
        }
        // Mirror what the device reports so the UI and tray agree with it.
        s.lights = status.lights;
        s.inactive = status.auto_off;
        s.noise_control = status.noise_control;
        (s.secs_per_step, s.low_battery_percent, s.notify_spare_ready)
    });

    let snapshot = Snapshot {
        connected: true,
        node,
        device: Some(status),
        error: None,
        minutes_left: discharging.then(|| clock.minutes_left(h.raw, secs_per_step)),
        estimate_learned: (secs_per_step - store::NOMINAL_SECS_PER_STEP).abs() > 1.0,
        updated: now_secs(),
    };

    let low = discharging
        && h.percent <= low_at
        && last_low.map(|t| t.elapsed() >= LOW_COOLDOWN).unwrap_or(true);
    if low {
        *last_low = Some(Instant::now());
        let swap = if status.spare.percent >= 75 {
            format!(" Your spare battery is at {}% — swap it in.", status.spare.percent)
        } else {
            String::new()
        };
        notify("Headset battery low", &format!("{}% left.{swap}", h.percent));
    }
    if !discharging {
        *last_low = None;
    }

    if notify_spare {
        if let Some(prev) = previous {
            if prev.spare.charging && status.spare.raw == arctis::STEPS {
                notify("Spare battery charged", "The battery in the base station is full.");
            }
        }
    }

    tray::update(app, &snapshot);
    let _ = app.emit("device", &snapshot);
    if let Ok(mut s) = engine.snapshot.lock() {
        *s = snapshot;
    }
}

fn publish_offline(app: &AppHandle, engine: &Engine, error: String) {
    let changed = engine
        .snapshot
        .lock()
        .map(|s| s.connected || s.error.as_deref() != Some(error.as_str()))
        .unwrap_or(true);
    if !changed {
        return;
    }
    let snapshot = Snapshot {
        error: Some(error),
        updated: now_secs(),
        ..Default::default()
    };
    tray::update(app, &snapshot);
    let _ = app.emit("device", &snapshot);
    if let Ok(mut s) = engine.snapshot.lock() {
        *s = snapshot;
    }
}

fn apply_dial(app: &AppHandle, inner: &Arc<Mutex<Inner>>, balance: i8) {
    let follow = with_settings(inner, |s| {
        if s.chatmix.enabled && s.chatmix.use_dial && s.chatmix.balance != balance {
            s.chatmix.balance = balance;
            store::save(s);
            true
        } else {
            false
        }
    });
    if follow {
        let _ = chatmix::apply_balance(balance);
        let (game, chat) = chatmix::levels(balance);
        let _ = app.emit(
            "chatmix",
            serde_json::json!({ "balance": balance, "game": game, "chat": chat }),
        );
    }
}

fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["-a", "Arctis Control", "-i", "audio-headset", title, body])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_needs_a_whole_step() {
        let mut c = BatteryClock::default();
        assert_eq!(c.observe(6, true), None); // first reading
        assert_eq!(c.observe(5, true), None); // partial step: start unknown
        assert!(c.step_started.is_some());
        let sample = c.observe(4, true);
        assert!(sample.is_some());
    }

    #[test]
    fn charging_resets_the_clock() {
        let mut c = BatteryClock::default();
        c.observe(6, true);
        c.observe(5, true);
        c.observe(5, false);
        assert!(c.step_started.is_none());
        assert_eq!(c.observe(4, true), None);
    }

    #[test]
    fn estimate_scales_with_steps() {
        let c = BatteryClock::default();
        let full = c.minutes_left(8, 9000.0);
        let half = c.minutes_left(4, 9000.0);
        assert!(full > half);
        assert_eq!(full, ((8.0 * 9000.0 - 4500.0) / 60.0f32).round() as u32);
    }
}
