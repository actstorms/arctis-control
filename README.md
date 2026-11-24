# Arctis Control

A Linux control panel for the SteelSeries Arctis Nova Pro Wireless — the headset
half of SteelSeries GG (and then some), without the account, telemetry or the
Windows-only engine. Rust + Tauri; talks to the base station over USB HID and
builds its audio features on PipeWire.

## Features

| Area | What you get |
| --- | --- |
| **Batteries** | Headset **and** spare battery (the one charging in the base station) with %, charging state, and an estimated time left that learns your real drain rate. "Time to swap" hint, low-battery and "spare charged" notifications. |
| **Noise control** | Active noise cancelling / Transparency / Off — from the window or the tray. Changes made with the headset button show up live. |
| **Studio microphone** | Virtual mic “Arctis Mic (Studio)”: AI noise removal (DeepFilterNet or RNNoise), noise gate, AGC leveler, compressor, depth/warmth/mud/clarity/air EQ, tube saturation, bass body, de-esser and look-ahead limiter. Six voice presets (Deep Radio, Natural, Broadcast, Podcast, Streaming, Gaming), every control applied **live**, live input/output meters, record-and-compare A/B test, listen-live monitor. |
| **Equalizer** | Interactive response curve (drag points, scroll for 0.5 dB, double-click to zero) plus 10 band sliders, 9 built-in curves, save/delete your own presets, bypass without losing your curve. Stored in the headset. |
| **ChatMix** | “Arctis Game” and “Arctis Chat” outputs mixed into the headset; balance from the app or the base-station dial; set either as default output. |
| **Headset** | Sidetone, auto power-off, base-station brightness, headset mic gain. Values the base station reports (brightness, auto-off, noise control) are read back, not guessed. |
| **App** | Tray icon with battery gauge, both batteries in the tooltip, quick toggles for ANC / studio mic / ChatMix; start at login; light and dark theme. |

Settings live in `~/.config/arctis-control/settings.json`.

## Build and run

```bash
sudo install -m644 packaging/99-steelseries-hidraw.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=hidraw

cargo build --release            # builds the app and libarctis_dsp.so
./target/release/arctis-control              # window + tray
./target/release/arctis-control --hidden     # tray only (used by autostart)
./target/release/arctis-control --page mic   # open on a page
./target/release/arctis-control --help       # all CLI options
```

Put it in your application menu (per user, no root):

```bash
install -Dm755 target/release/arctis-control        ~/.local/bin/arctis-control
install -Dm755 target/release/libarctis_dsp.so      ~/.local/lib/arctis-control/libarctis_dsp.so
install -Dm644 icons/icon.png                       ~/.local/share/icons/hicolor/256x256/apps/arctis-control.png
install -Dm644 packaging/arctis-control.desktop     ~/.local/share/applications/arctis-control.desktop
update-desktop-database ~/.local/share/applications
```

The app looks for `libarctis_dsp.so` next to its binary, then in
`../lib/arctis-control`, then `/usr/{local/,}lib/arctis-control`.

Headless use:

```bash
arctis-control --dump                         # device status as JSON
arctis-control --anc on|transparency|off
arctis-control --mic-on [natural|broadcast|podcast|streaming|gaming]
arctis-control --mic-off | --mic-status
arctis-control --chatmix on|off|<-100..100>   # -100 = all game
arctis-control --monitor                      # raw vendor HID frames
```

Requirements: Rust, GTK3 + WebKitGTK 4.1, PipeWire with `pactl`, `wpctl`,
`pw-cli`, `pw-dump`, `pw-record`/`pw-play`/`pw-loopback` and `systemctl --user`.
Recommended: `deepfilternet-ladspa` (best AI noise removal); `noise-suppression-for-voice`
(RNNoise) works as a fallback. Without either, the rest of the chain still works.

## How the audio features work

Each feature runs as **its own small `pipewire -c <conf>` process** under a
`systemd --user` unit (`arctis-mic.service`, `arctis-chatmix.service`) tied to
`pipewire.service`. Turning a feature on or off never restarts the main PipeWire,
so other audio does not drop out, and both keep working with the window closed.

Studio mic graph (PipeWire filter-chain):

```
hw mic ─ high-pass ─┬─ DeepFilterNet | RNNoise ─┐
                    └──────── dry ──────────────┴─ mix ─ gate
  ─ breath reducer ─ leveler (AGC) ─ compressor      dynamics first …
  ─ warmth ─ depth ─ mud ─ clarity ─ air             … tone after
  ─ tube / bass body ─ de-esser ─ limiter ─> Arctis Mic (Studio)
```

* **Noise removal:** DeepFilterNet (`deepfilternet-ladspa`, preferred) or
  RNNoise (`noise-suppression-for-voice`), chosen in the app.
* **Breath reducer:** ducks breathing through mouth or nose before the
  leveler and compressor, which would otherwise lift it. A breath is detected
  as *unvoiced* (no pitch, via centre-clipped autocorrelation), clearly quieter
  than recent speech, and not a sibilant (it has energy below 2.5 kHz, while
  "s"/"sh" have almost none), lasting at least 60 ms. The audio runs 30 ms
  behind the detector, so the duck lifts before the next word. A consonant
  onset ("f", "h", "th") also ends it, and the detector stays disarmed until
  voiced speech, so word starts are kept. Measured: breaths that get past
  noise removal go from ~15 dB to ~30 dB below speech, with speech unchanged.
* **Leveler:** a slow AGC that brings speech to about −18 dBFS before the
  compressor, with its gain frozen below the gate threshold so pauses and room
  noise are never pumped up — it only adapts within 9 dB of your recent speech
  level, so pauses and breaths are never ridden up. Leaning back from the mic
  no longer changes your level.
* **Compressor:** near-peak detection and soft knee; this is what gives the
  dense "radio" sound.
* **Tone after dynamics:** compressing after a bass boost would turn the
  boosted vowels back down, so depth (150 Hz), warmth (200 Hz shelf), mud
  (500 Hz), clarity (3.8 kHz) and air (10 kHz) come after the compressor.
* **Tube / bass body:** gain-normalised asymmetric saturation (even
  harmonics), plus psychoacoustic low harmonics that a small headset capsule
  cannot capture.

Measured on test speech for the **Deep Radio** preset: chest band (60–180 Hz)
+6 to +10 dB against the mids, a −3 to −4.5 dB scoop at 500–1400 Hz, presence
+4.6 dB at 4 kHz, crest factor 9.7 dB (from 11.1), about −15 dBFS RMS. Input
14 dB quieter comes out within 1 dB of normal. CPU is about 3 % while in use.

Every stage is always in the graph and neutral at its "off" setting, so presets
and sliders are pushed to the running graph with
`pw-cli set-param … Props { params = [...] }`, with no restart. Only switching
the noise engine restarts the (separate) mic service. The gate, breath reducer,
leveler, compressor, tube/body, de-esser and limiter are this project's own LADSPA plugin
(`src/dsp.rs`, `src/ladspa.rs` → `libarctis_dsp.so`), because PipeWire ships no
dynamics processors. The app copies it to
`~/.local/share/arctis-control/libarctis_dsp.so` so a `cargo clean` cannot pull
it out from under PipeWire. All seven processors are bit-exact pass-through at
their neutral settings and unit-tested.

The first version wrote `~/.config/pipewire/pipewire.conf.d/90-arctis-mic.conf`;
it is migrated (removed, one PipeWire restart) the first time this version runs.

## Protocol

Frames are 64-byte HID reports on interface `1.4` (vendor page `0xffc0`):
`[0x06][command][payload…]`, replies with report id `0x06`, unsolicited events
with `0x07`. Every setting is followed by `0x09` (save).

| Setting | Command | Payload | Read back in status |
| --- | --- | --- | --- |
| Status query | `0xb0` | — | — |
| Noise control | `0xbd` | 0 off, 1 transparency, 2 ANC | `[10]` ✓ |
| Brightness | `0xbf` | 1–10 | `[11]` ✓ |
| Auto power-off | `0xc1` | 0–6 → never/1/5/10/15/30/60 min | `[12]` ✓ |
| Mic gain | `0x37` | 1–10 | no (verified acoustically, ≈17 dB range) |
| Sidetone | `0x39` | 0–3 | no |
| EQ preset | `0x2e` | 0–3 onboard, 4 custom | no |
| EQ bands | `0x33` | 10 bytes, `0x14 + 2·dB` | no |
| ChatMix dial reporting | `0x49` | 0/1 | dial events `07 45 <game> <chat>` |

Status bytes: `[6]` headset battery 0–8, `[7]` spare battery 0–8, `[15]`
`0x01` headset off / `0x02` charging / `0x08` on.

Live reply from the connected headset (75 % headset, full spare, ANC on,
brightness 10, auto-off 30 min):

```
06 b0 00 00 04 04 06 08 08 00 02 0a 05 01 08 08 …
```

### How it was established (REA)

* GG's per-device programs (`apps/engine/deviceSpecifications/*.edevice`) are
  encrypted, so opcodes cannot be read from the installer. The retained GG
  engine strings (stripped Go binary, module
  `github.com/steelseries/engine/cmd/ssenext`, REA `inspect_go_binary`, evidence
  `ev_665d8b48…123ef7`) do name the feature model for this headset:
  `deviceArctisNovaProWirelessTx`, `AncMode`/`AncLevel`, `MicNoiseReduction`,
  `Chatmix` (`chatmix_game_level` / `chatmix_chat_level`),
  `PrimaryAndBackupBatteryStatus`, `arctisNovaProWirelessLegacyEQHeadset`
  (10-band, not parametric). That set the feature checklist and confirmed the
  base station reports a backup battery.
* Each opcode in the table marked ✓ was then verified on hardware by writing a
  neighbouring value, reading the status frame back, and restoring the original.
  Mic gain was verified by recording the mic at gain 1 / 4 / 7 / 10
  (−85 → −68 dBFS). The `[7]` spare-battery byte is inferred from GG's
  primary/backup model and its position next to `[6]`.
* `SteelSeries.GG.DeviceIO.dll` is only the Win32 HID wrapper (REA
  `inspect_managed_members`, evidence `ev_c57cb88e…b4d1a`).

The ChatMix dial event layout (`07 45 game chat`) follows public community
documentation and has not yet been captured from this unit — run
`arctis-control --monitor` and turn the dial to confirm. The ChatMix sliders in
the app work regardless. No HeadsetControl code is used (GPL-2.0; this is MIT).

### Still open

Transparency level (`AncLevel`), mic-mute reporting, Bluetooth settings and
firmware version: GG has them, but their opcodes are only in the encrypted
`.edevice` programs and are not reflected in the status frame. Press the
corresponding buttons while `--monitor` runs to find their event frames.

## Layout

```
src/arctis.rs    HID protocol, status/event parsing, device discovery
src/engine.rs    background device thread: events, battery learning, notifications
src/mic.rs       studio mic chain (graph, presets, live params, mic test)
src/chatmix.rs   Game/Chat outputs and balance
src/audio.rs     PipeWire user services, node lookup, meters, helper processes
src/tray.rs      tray gauge, tooltip, quick toggles
src/store.rs     settings + autostart
src/main.rs      Tauri commands + CLI
src/lib.rs       libarctis_dsp.so (dsp.rs algorithms, ladspa.rs ABI)
ui/              HTML/CSS/JS front end (no npm, no bundler)
capabilities/    Tauri permissions (events for live updates)
packaging/       udev rule, desktop entry
```

## License

MIT — see [LICENSE](LICENSE).
