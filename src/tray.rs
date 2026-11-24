//! System tray: a battery gauge icon, a tooltip with both batteries, and
//! quick toggles for noise control, the studio mic and ChatMix.

use crate::engine::Snapshot;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "battery";

pub struct TrayItems {
    anc: [CheckMenuItem<Wry>; 3],
    mic: CheckMenuItem<Wry>,
    chatmix: CheckMenuItem<Wry>,
}

pub fn setup(app: &AppHandle, mic_on: bool, chatmix_on: bool) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open Arctis Control", true, None::<&str>)?;
    let anc = [
        CheckMenuItem::with_id(app, "nc:2", "Noise cancelling", true, false, None::<&str>)?,
        CheckMenuItem::with_id(app, "nc:1", "Transparency", true, false, None::<&str>)?,
        CheckMenuItem::with_id(app, "nc:0", "Noise control off", true, false, None::<&str>)?,
    ];
    let mic = CheckMenuItem::with_id(app, "mic", "Studio microphone", true, mic_on, None::<&str>)?;
    let chatmix = CheckMenuItem::with_id(app, "chatmix", "ChatMix", true, chatmix_on, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &show, &sep1, &anc[0], &anc[1], &anc[2], &sep2, &mic, &chatmix, &sep3, &quit,
        ],
    )?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(battery_icon(None, false))
        .tooltip("Arctis Control")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let id = event.id.as_ref().to_string();
            match id.as_str() {
                "show" => show_window(app),
                "quit" => crate::quit(app),
                _ => {
                    let app = app.clone();
                    std::thread::spawn(move || crate::tray_action(&app, &id));
                }
            }
        })
        .build(app)?;

    app.manage(TrayItems { anc, mic, chatmix });
    Ok(())
}

pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn set_feature_checks(app: &AppHandle, mic: bool, chatmix: bool) {
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.mic.set_checked(mic);
        let _ = items.chatmix.set_checked(chatmix);
    }
}

pub fn tooltip(s: &Snapshot) -> String {
    let Some(d) = s.device else {
        return format!(
            "Arctis Control — {}",
            s.error.as_deref().unwrap_or("base station not found")
        );
    };
    let h = d.headset;
    let headset = if !h.online {
        "headset off".to_string()
    } else if h.charging {
        format!("headset {}% (charging)", h.percent)
    } else {
        match s.minutes_left {
            Some(m) => format!("headset {}% (~{})", h.percent, duration(m)),
            None => format!("headset {}%", h.percent),
        }
    };
    let spare = if !d.spare.present {
        "no spare battery".to_string()
    } else if d.spare.charging {
        format!("spare {}% (charging)", d.spare.percent)
    } else {
        format!("spare {}%", d.spare.percent)
    };
    format!("Arctis Nova Pro — {headset} · {spare}")
}

pub fn duration(minutes: u32) -> String {
    if minutes >= 60 {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

pub fn update(app: &AppHandle, s: &Snapshot) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let (pct, charging) = match s.device {
            Some(d) if d.headset.online => (Some(d.headset.percent), d.headset.charging),
            _ => (None, false),
        };
        let _ = tray.set_icon(Some(battery_icon(pct, charging)));
        let _ = tray.set_tooltip(Some(tooltip(s)));
    }
    if let (Some(items), Some(d)) = (app.try_state::<TrayItems>(), s.device) {
        for (item, mode) in items.anc.iter().zip([2u8, 1, 0]) {
            let _ = item.set_checked(d.noise_control == mode);
        }
    }
}

/// 32x32 gauge: outlined body, nub, fill proportional to charge. `None`
/// draws an empty grey gauge for "headset off / not connected".
pub fn battery_icon(percent: Option<u8>, charging: bool) -> Image<'static> {
    const OUTLINE: [u8; 4] = [232, 236, 240, 255];
    const DIM: [u8; 4] = [120, 126, 136, 255];
    const EMPTY: [u8; 4] = [58, 62, 70, 255];
    let outline = if percent.is_some() { OUTLINE } else { DIM };
    let pct = percent.unwrap_or(0);
    let fill = if charging {
        [61, 220, 132, 255]
    } else if pct <= 25 {
        [255, 77, 79, 255]
    } else {
        [255, 90, 54, 255]
    };

    let mut px = vec![0u8; 32 * 32 * 4];
    for x in 3..=26 {
        put(&mut px, x, 9, outline);
        put(&mut px, x, 22, outline);
    }
    for y in 9..=22 {
        put(&mut px, 3, y, outline);
        put(&mut px, 26, y, outline);
    }
    for x in 27..=29 {
        for y in 13..=18 {
            put(&mut px, x, y, outline);
        }
    }
    for x in 5..=24 {
        for y in 11..=20 {
            put(&mut px, x, y, EMPTY);
        }
    }
    let width = i32::from(pct.min(100)) * 20 / 100;
    for x in 5..5 + width {
        for y in 11..=20 {
            put(&mut px, x, y, fill);
        }
    }

    Image::new_owned(px, 32, 32)
}

fn put(px: &mut [u8], x: i32, y: i32, color: [u8; 4]) {
    if !(0..32).contains(&x) || !(0..32).contains(&y) {
        return;
    }
    let i = ((y * 32 + x) * 4) as usize;
    px[i..i + 4].copy_from_slice(&color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arctis::parse_status;

    fn pixel(image: &Image<'_>, x: u32, y: u32) -> [u8; 4] {
        let raw = image.rgba();
        let i = ((y * 32 + x) * 4) as usize;
        raw[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn gauge_fill_tracks_percentage() {
        let empty = battery_icon(Some(0), false);
        let full = battery_icon(Some(100), false);
        assert_eq!(pixel(&empty, 6, 15), [58, 62, 70, 255]);
        assert_eq!(pixel(&full, 6, 15), [255, 90, 54, 255]);
        assert_eq!(pixel(&full, 24, 15), [255, 90, 54, 255]);
    }

    #[test]
    fn charging_uses_green_and_low_uses_red() {
        assert_eq!(pixel(&battery_icon(Some(50), true), 6, 15), [61, 220, 132, 255]);
        assert_eq!(pixel(&battery_icon(Some(25), false), 6, 15), [255, 77, 79, 255]);
    }

    #[test]
    fn tooltip_names_both_batteries() {
        let frame = [6, 0xb0, 0, 0, 4, 4, 6, 3, 8, 0, 2, 10, 5, 1, 8, 8];
        let s = Snapshot {
            connected: true,
            device: parse_status(&frame),
            minutes_left: Some(125),
            ..Default::default()
        };
        assert_eq!(
            tooltip(&s),
            "Arctis Nova Pro — headset 75% (~2h 05m) · spare 37% (charging)"
        );
    }
}
