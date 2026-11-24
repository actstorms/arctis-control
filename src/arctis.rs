//! Wire protocol for the SteelSeries Arctis Nova Pro Wireless base station.
//!
//! Frames are 64-byte HID reports on interface 4 (vendor usage page 0xffc0,
//! report id 0x06 for commands, 0x07 for unsolicited events):
//!   [0]=0x06 report id, [1]=command, [2..]=payload
//!
//! Status reply to `0xb0` (fields marked ✓ were verified on hardware by
//! writing the setting and reading the byte back, see README):
//!
//! | byte | meaning                                           |
//! | ---- | ------------------------------------------------- |
//! | 6    | headset battery, 0-8 ✓                            |
//! | 7    | spare battery in the base-station slot, 0-8       |
//! | 10   | noise control: 0 off, 1 transparency, 2 ANC ✓     |
//! | 11   | base-station brightness 1-10 ✓                    |
//! | 12   | auto power-off index 0-6 ✓                        |
//! | 15   | 0x01 headset off, 0x02 charging (cable), 0x08 on ✓|
//!
//! No HeadsetControl code is copied here.

use serde::Serialize;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const VENDOR: u16 = 0x1038;
const PRODUCTS: [u16; 2] = [0x12e0, 0x12e5];
const REPORT_ID: u8 = 0x06;
const EVENT_ID: u8 = 0x07;
const REPORT_LEN: usize = 64;
const READ_TIMEOUT: Duration = Duration::from_millis(800);

pub const STEPS: u8 = 8;

const CMD_SAVE: u8 = 0x09;
const CMD_EQ_PRESET: u8 = 0x2e;
const CMD_EQ_BANDS: u8 = 0x33;
const CMD_MIC_VOLUME: u8 = 0x37;
const CMD_SIDETONE: u8 = 0x39;
const CMD_CHATMIX_REPORTING: u8 = 0x49;
const CMD_STATUS: u8 = 0xb0;
const CMD_NOISE_CONTROL: u8 = 0xbd;
const CMD_LIGHTS: u8 = 0xbf;
const CMD_AUTO_OFF: u8 = 0xc1;
/// Unsolicited ChatMix dial position: `[0x07, 0x45, game 0-100, chat 0-100]`.
const EVT_CHATMIX: u8 = 0x45;

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// Headset is powered on and linked to the base station.
    pub online: bool,
    /// Headset is charging over its USB cable.
    pub charging: bool,
    /// Battery gauge steps reported by the base station (0-8).
    pub raw: u8,
    pub percent: u8,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub struct SpareBattery {
    pub raw: u8,
    pub percent: u8,
    /// The slot charges whenever the base station has power, until full.
    pub charging: bool,
    /// A reading of 0 is what an empty slot reports too.
    pub present: bool,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub struct DeviceStatus {
    pub headset: Battery,
    pub spare: SpareBattery,
    /// 0 off, 1 transparency, 2 active noise cancelling.
    pub noise_control: u8,
    pub lights: u8,
    pub auto_off: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    ChatMix { game: u8, chat: u8 },
    /// Any other unsolicited report (button presses, mode changes): the
    /// caller should re-read the status.
    Changed,
}

pub fn percent(raw: u8) -> u8 {
    (u32::from(raw.min(STEPS)) * 100 / u32::from(STEPS)) as u8
}

pub fn parse_status(r: &[u8]) -> Option<DeviceStatus> {
    if r.len() < 16 || r[0] != REPORT_ID || r[1] != CMD_STATUS {
        return None;
    }
    let state = r[15];
    let raw = r[6].min(STEPS);
    let spare = r[7].min(STEPS);
    Some(DeviceStatus {
        headset: Battery {
            online: state == 0x08 || state == 0x02,
            charging: state == 0x02,
            raw,
            percent: percent(raw),
        },
        spare: SpareBattery {
            raw: spare,
            percent: percent(spare),
            charging: spare > 0 && spare < STEPS,
            present: spare > 0,
        },
        noise_control: r[10].min(2),
        lights: r[11].clamp(1, 10),
        auto_off: r[12].min(6),
    })
}

pub fn parse_event(r: &[u8]) -> Option<Event> {
    if r.len() < 4 || r[0] != EVENT_ID {
        return None;
    }
    if r[1] == EVT_CHATMIX {
        return Some(Event::ChatMix {
            game: r[2].min(100),
            chat: r[3].min(100),
        });
    }
    Some(Event::Changed)
}

/// ±10 dB in 0.5 dB steps, centred on 0x14.
pub fn eq_payload(bands: &[f32; 10]) -> [u8; 10] {
    let mut out = [0u8; 10];
    for (o, db) in out.iter_mut().zip(bands) {
        *o = (20.0 + 2.0 * db.clamp(-10.0, 10.0)).round() as u8;
    }
    out
}

pub struct Arctis {
    file: fs::File,
    pub node: PathBuf,
}

impl Arctis {
    pub fn open() -> Result<Self, String> {
        let node = find_node().ok_or("Arctis base station not found")?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&node)
            .map_err(|e| {
                if e.kind() == ErrorKind::PermissionDenied {
                    format!(
                        "no permission for {} — install packaging/99-steelseries-hidraw.rules",
                        node.display()
                    )
                } else {
                    format!("open {}: {e}", node.display())
                }
            })?;
        Ok(Self { file, node })
    }

    fn send(&mut self, cmd: u8, payload: &[u8]) -> Result<(), String> {
        let mut buf = [0u8; REPORT_LEN];
        buf[0] = REPORT_ID;
        buf[1] = cmd;
        let n = payload.len().min(REPORT_LEN - 2);
        buf[2..2 + n].copy_from_slice(&payload[..n]);
        self.file
            .write_all(&buf)
            .map_err(|e| format!("hid write: {e}"))
    }

    /// Skips the unsolicited 0x07 events and other replies queued on this
    /// handle until the wanted reply arrives.
    fn wait_for(&mut self, want: fn(&[u8]) -> bool) -> Result<[u8; REPORT_LEN], String> {
        let deadline = Instant::now() + READ_TIMEOUT;
        let mut buf = [0u8; REPORT_LEN];
        loop {
            match self.file.read(&mut buf) {
                Ok(n) if n > 0 => {
                    if want(&buf[..n]) {
                        return Ok(buf);
                    }
                }
                Ok(_) => return Err("hid read: device closed".into()),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err("timeout waiting for device reply".into());
                    }
                    std::thread::sleep(Duration::from_millis(4));
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("hid read: {e}")),
            }
        }
    }

    pub fn status(&mut self) -> Result<DeviceStatus, String> {
        self.send(CMD_STATUS, &[])?;
        let r = self.wait_for(|b| parse_status(b).is_some())?;
        parse_status(&r).ok_or_else(|| "malformed status reply".into())
    }

    /// Non-blocking read of every report currently queued.
    pub fn drain(&mut self) -> Result<Vec<[u8; REPORT_LEN]>, String> {
        let mut frames = Vec::new();
        let mut buf = [0u8; REPORT_LEN];
        loop {
            match self.file.read(&mut buf) {
                Ok(n) if n > 0 => frames.push(buf),
                Ok(_) => return Err("hid read: device closed".into()),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(frames),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("hid read: {e}")),
            }
        }
    }

    fn save(&mut self) -> Result<(), String> {
        self.send(CMD_SAVE, &[])
    }

    /// 0 = off, 1..3 = low..high
    pub fn set_sidetone(&mut self, level: u8) -> Result<(), String> {
        self.send(CMD_SIDETONE, &[level.min(3)])?;
        self.save()
    }

    /// Base station LED / OLED brightness, 1..10
    pub fn set_lights(&mut self, level: u8) -> Result<(), String> {
        self.send(CMD_LIGHTS, &[level.clamp(1, 10)])?;
        self.save()
    }

    /// 0 = never, 1 = 1 min, 2 = 5, 3 = 10, 4 = 15, 5 = 30, 6 = 60
    pub fn set_auto_off(&mut self, level: u8) -> Result<(), String> {
        self.send(CMD_AUTO_OFF, &[level.min(6)])?;
        self.save()
    }

    /// 0 = off, 1 = transparency, 2 = active noise cancelling
    pub fn set_noise_control(&mut self, mode: u8) -> Result<(), String> {
        self.send(CMD_NOISE_CONTROL, &[mode.min(2)])?;
        self.save()
    }

    /// Hardware microphone gain, 1..10 (≈ 17 dB between 1 and 10).
    pub fn set_mic_volume(&mut self, level: u8) -> Result<(), String> {
        self.send(CMD_MIC_VOLUME, &[level.clamp(1, 10)])?;
        self.save()
    }

    /// Ask the base station to report ChatMix dial turns as 0x07/0x45 events.
    pub fn set_chatmix_reporting(&mut self, on: bool) -> Result<(), String> {
        self.send(CMD_CHATMIX_REPORTING, &[u8::from(on)])
    }

    /// Ten bands, -10.0..=+10.0 dB. Selects the custom preset slot first.
    pub fn set_equalizer(&mut self, bands: &[f32; 10]) -> Result<(), String> {
        self.send(CMD_EQ_PRESET, &[4])?;
        self.send(CMD_EQ_BANDS, &eq_payload(bands))?;
        self.save()
    }
}

fn find_node() -> Option<PathBuf> {
    let mut candidates: Vec<(bool, PathBuf)> = Vec::new();
    for entry in fs::read_dir("/sys/class/hidraw").ok()?.flatten() {
        let node = PathBuf::from("/dev").join(entry.file_name());
        let Ok(uevent) = fs::read_to_string(entry.path().join("device/uevent")) else {
            continue;
        };
        let matches = uevent.lines().any(|line| {
            line.strip_prefix("HID_ID=0003:")
                .and_then(|rest| rest.split_once(':'))
                .map(|(v, p)| {
                    u16::from_str_radix(v, 16).ok() == Some(VENDOR)
                        && u16::from_str_radix(p, 16)
                            .map(|p| PRODUCTS.contains(&p))
                            .unwrap_or(false)
                })
                .unwrap_or(false)
        });
        if !matches {
            continue;
        }
        // Interface 4 is the vendor control interface; interface 3 is the
        // consumer-control (media key) one and answers nothing.
        let is_control = fs::read_link(entry.path())
            .map(|p| p.to_string_lossy().contains(":1.4"))
            .unwrap_or(false);
        candidates.push((is_control, node));
    }
    candidates.sort_by_key(|(control, _)| !*control);
    candidates.into_iter().next().map(|(_, node)| node)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from the connected base station (headset on battery, ANC on).
    const LIVE: [u8; 16] = [
        0x06, 0xb0, 0x00, 0x00, 0x04, 0x04, 0x06, 0x08, 0x08, 0x00, 0x02, 0x0a, 0x05, 0x01, 0x08,
        0x08,
    ];

    #[test]
    fn parses_live_status_frame() {
        let s = parse_status(&LIVE).unwrap();
        assert_eq!(s.headset.raw, 6);
        assert_eq!(s.headset.percent, 75);
        assert!(s.headset.online && !s.headset.charging);
        assert_eq!(s.spare.percent, 100);
        assert!(!s.spare.charging && s.spare.present);
        assert_eq!(s.noise_control, 2);
        assert_eq!(s.lights, 10);
        assert_eq!(s.auto_off, 5);
    }

    #[test]
    fn spare_below_full_is_charging() {
        let mut frame = LIVE;
        frame[7] = 3;
        frame[15] = 0x02;
        let s = parse_status(&frame).unwrap();
        assert!(s.spare.charging);
        assert_eq!(s.spare.percent, 37);
        assert!(s.headset.charging && s.headset.online);
    }

    #[test]
    fn rejects_other_reports() {
        assert!(parse_status(&[0x07, 0xb0, 0, 0]).is_none());
        assert!(parse_status(&LIVE[..10]).is_none());
    }

    #[test]
    fn chatmix_event() {
        assert_eq!(
            parse_event(&[0x07, 0x45, 100, 40]),
            Some(Event::ChatMix { game: 100, chat: 40 })
        );
        assert_eq!(parse_event(&[0x07, 0xbd, 1, 0]), Some(Event::Changed));
        assert_eq!(parse_event(&LIVE), None);
    }

    #[test]
    fn eq_byte_layout_matches_frame() {
        let bands = [-10.0, -2.5, 0.0, 0.5, 10.0, 1.0, 2.0, 3.0, 4.0, -12.0];
        let p = eq_payload(&bands);
        assert_eq!(p[0], 0x00);
        assert_eq!(p[1], 0x0f);
        assert_eq!(p[2], 0x14);
        assert_eq!(p[3], 0x15);
        assert_eq!(p[4], 0x28);
        assert_eq!(p[9], 0x00);
    }
}
