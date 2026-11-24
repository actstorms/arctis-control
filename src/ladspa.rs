//! Hand-written LADSPA 1.1 ABI (the header is tiny and stable) exposing the
//! processors in `dsp.rs`. Port names are the keys PipeWire's filter-chain
//! uses in `control = { ... }` and in runtime `params` updates.

use crate::dsp::{
    Agc, AgcParams, Breath, BreathParams, CompParams, Compressor, Deesser, DeesserParams, Gate, GateParams, Limiter,
    LimiterParams, Warmth, WarmthParams,
};
use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::sync::OnceLock;

type Data = f32;
type Handle = *mut c_void;

const PORT_INPUT: c_int = 0x1;
const PORT_OUTPUT: c_int = 0x2;
const PORT_CONTROL: c_int = 0x4;
const PORT_AUDIO: c_int = 0x8;

const HINT_BELOW: c_int = 0x1;
const HINT_ABOVE: c_int = 0x2;
const HINT_LOG: c_int = 0x10;
const HINT_DEFAULT_MIN: c_int = 0x40;
const HINT_DEFAULT_LOW: c_int = 0x80;
const HINT_DEFAULT_MID: c_int = 0xC0;
const HINT_DEFAULT_HIGH: c_int = 0x100;
const HINT_DEFAULT_MAX: c_int = 0x140;
const HINT_DEFAULT_0: c_int = 0x200;
const HINT_DEFAULT_1: c_int = 0x240;

const PROP_HARD_RT: c_int = 0x4;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RangeHint {
    hint: c_int,
    lower: Data,
    upper: Data,
}

#[repr(C)]
pub struct Descriptor {
    unique_id: c_ulong,
    label: *const c_char,
    properties: c_int,
    name: *const c_char,
    maker: *const c_char,
    copyright: *const c_char,
    port_count: c_ulong,
    port_descriptors: *const c_int,
    port_names: *const *const c_char,
    port_range_hints: *const RangeHint,
    implementation_data: *mut c_void,
    instantiate: extern "C" fn(*const Descriptor, c_ulong) -> Handle,
    connect_port: extern "C" fn(Handle, c_ulong, *mut Data),
    activate: Option<extern "C" fn(Handle)>,
    run: extern "C" fn(Handle, c_ulong),
    run_adding: Option<extern "C" fn(Handle, c_ulong)>,
    set_run_adding_gain: Option<extern "C" fn(Handle, Data)>,
    deactivate: Option<extern "C" fn(Handle)>,
    cleanup: extern "C" fn(Handle),
}

// Descriptors only point at 'static data.
unsafe impl Send for Descriptor {}
unsafe impl Sync for Descriptor {}

/// A control port: NUL-terminated name, range, LADSPA default hint, and the
/// value the processor actually uses before the host connects the port.
struct Control {
    name: &'static str,
    lo: f32,
    hi: f32,
    hint: c_int,
    default: f32,
}

const fn ctl(name: &'static str, lo: f32, hi: f32, hint: c_int, default: f32) -> Control {
    Control { name, lo, hi, hint, default }
}

trait Plugin: Sized + 'static {
    const ID: c_ulong;
    const LABEL: &'static str;
    const NAME: &'static str;
    const CONTROLS: &'static [Control];
    fn new(rate: f32) -> Self;
    fn reset(&mut self);
    fn run(&mut self, controls: &[f32], input: &[f32], output: &mut [f32]);
}

/// Instance: two audio ports (0 = In, 1 = Out) then the controls.
struct Instance<P: Plugin> {
    dsp: P,
    input: *const Data,
    output: *mut Data,
    controls: [*const Data; 8],
    values: [f32; 8],
}

extern "C" fn instantiate<P: Plugin>(_: *const Descriptor, rate: c_ulong) -> Handle {
    let mut values = [0.0; 8];
    for (v, c) in values.iter_mut().zip(P::CONTROLS) {
        *v = c.default;
    }
    let inst = Box::new(Instance::<P> {
        dsp: P::new(rate as f32),
        input: std::ptr::null(),
        output: std::ptr::null_mut(),
        controls: [std::ptr::null(); 8],
        values,
    });
    Box::into_raw(inst) as Handle
}

extern "C" fn connect_port<P: Plugin>(h: Handle, port: c_ulong, data: *mut Data) {
    // SAFETY: `h` was produced by `instantiate::<P>`.
    let inst = unsafe { &mut *(h as *mut Instance<P>) };
    match port {
        0 => inst.input = data,
        1 => inst.output = data,
        n => {
            if let Some(slot) = inst.controls.get_mut(n as usize - 2) {
                *slot = data;
            }
        }
    }
}

extern "C" fn activate<P: Plugin>(h: Handle) {
    // SAFETY: see `connect_port`.
    let inst = unsafe { &mut *(h as *mut Instance<P>) };
    inst.dsp.reset();
}

extern "C" fn run<P: Plugin>(h: Handle, n: c_ulong) {
    // SAFETY: see `connect_port`; the host guarantees the connected buffers
    // hold `n` samples for the duration of this call.
    let inst = unsafe { &mut *(h as *mut Instance<P>) };
    if inst.input.is_null() || inst.output.is_null() || n == 0 {
        return;
    }
    for (i, c) in P::CONTROLS.iter().enumerate() {
        let ptr = inst.controls[i];
        if !ptr.is_null() {
            let v = unsafe { *ptr };
            inst.values[i] = if v.is_finite() { v.clamp(c.lo, c.hi) } else { c.default };
        }
    }
    let n = n as usize;
    // Hosts may process in place, so copy the input before writing output.
    let mut scratch = [0.0f32; 1024];
    let mut done = 0;
    while done < n {
        let len = (n - done).min(scratch.len());
        unsafe {
            std::ptr::copy_nonoverlapping(inst.input.add(done), scratch.as_mut_ptr(), len);
            let out = std::slice::from_raw_parts_mut(inst.output.add(done), len);
            inst.dsp.run(&inst.values[..P::CONTROLS.len()], &scratch[..len], out);
        }
        done += len;
    }
}

extern "C" fn cleanup<P: Plugin>(h: Handle) {
    if !h.is_null() {
        // SAFETY: matches the Box::into_raw in `instantiate`.
        drop(unsafe { Box::from_raw(h as *mut Instance<P>) });
    }
}

struct Built {
    descriptor: Descriptor,
    _port_descriptors: Vec<c_int>,
    _names: Vec<Vec<u8>>,
    _name_ptrs: Vec<*const c_char>,
    _hints: Vec<RangeHint>,
    _strings: [Vec<u8>; 3],
}

unsafe impl Send for Built {}
unsafe impl Sync for Built {}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

fn build<P: Plugin>() -> Built {
    let mut port_descriptors = vec![PORT_INPUT | PORT_AUDIO, PORT_OUTPUT | PORT_AUDIO];
    let mut names = vec![cstr("In"), cstr("Out")];
    let mut hints = vec![RangeHint { hint: 0, lower: 0.0, upper: 0.0 }; 2];
    for c in P::CONTROLS {
        port_descriptors.push(PORT_INPUT | PORT_CONTROL);
        names.push(cstr(c.name));
        hints.push(RangeHint {
            hint: HINT_BELOW | HINT_ABOVE | c.hint,
            lower: c.lo,
            upper: c.hi,
        });
    }
    let name_ptrs: Vec<*const c_char> = names.iter().map(|n| n.as_ptr() as *const c_char).collect();
    let strings = [cstr(P::LABEL), cstr(P::NAME), cstr("Arctis Control")];
    let descriptor = Descriptor {
        unique_id: P::ID,
        label: strings[0].as_ptr() as *const c_char,
        properties: PROP_HARD_RT,
        name: strings[1].as_ptr() as *const c_char,
        maker: strings[2].as_ptr() as *const c_char,
        copyright: strings[2].as_ptr() as *const c_char,
        port_count: port_descriptors.len() as c_ulong,
        port_descriptors: port_descriptors.as_ptr(),
        port_names: name_ptrs.as_ptr(),
        port_range_hints: hints.as_ptr(),
        implementation_data: std::ptr::null_mut(),
        instantiate: instantiate::<P>,
        connect_port: connect_port::<P>,
        activate: Some(activate::<P>),
        run: run::<P>,
        run_adding: None,
        set_run_adding_gain: None,
        deactivate: None,
        cleanup: cleanup::<P>,
    };
    Built {
        descriptor,
        _port_descriptors: port_descriptors,
        _names: names,
        _name_ptrs: name_ptrs,
        _hints: hints,
        _strings: strings,
    }
}

// ------------------------------------------------------------ plugins ---

impl Plugin for Gate {
    const ID: c_ulong = 0x4A7C_0001;
    const LABEL: &'static str = "arctis_gate";
    const NAME: &'static str = "Arctis Noise Gate";
    const CONTROLS: &'static [Control] = &[
        ctl("Threshold (dB)", -90.0, 0.0, HINT_DEFAULT_MID, -50.0),
        ctl("Range (dB)", -90.0, 0.0, HINT_DEFAULT_0, 0.0),
        ctl("Attack (ms)", 0.05, 50.0, HINT_DEFAULT_MIN, 1.0),
        ctl("Hold (ms)", 0.0, 1000.0, HINT_DEFAULT_LOW, 150.0),
        ctl("Release (ms)", 1.0, 2000.0, HINT_DEFAULT_LOW, 180.0),
    ];
    fn new(rate: f32) -> Self {
        Gate::new(rate)
    }
    fn reset(&mut self) {
        Gate::reset(self)
    }
    fn run(&mut self, c: &[f32], input: &[f32], output: &mut [f32]) {
        let p = GateParams {
            threshold_db: c[0],
            range_db: c[1],
            attack_ms: c[2],
            hold_ms: c[3],
            release_ms: c[4],
        };
        self.process(&p, input, output);
    }
}

impl Plugin for Compressor {
    const ID: c_ulong = 0x4A7C_0002;
    const LABEL: &'static str = "arctis_comp";
    const NAME: &'static str = "Arctis Compressor";
    const CONTROLS: &'static [Control] = &[
        ctl("Threshold (dB)", -60.0, 0.0, HINT_DEFAULT_HIGH, -18.0),
        ctl("Ratio", 1.0, 20.0, HINT_DEFAULT_1 | HINT_LOG, 1.0),
        ctl("Knee (dB)", 0.0, 24.0, HINT_DEFAULT_LOW, 6.0),
        ctl("Attack (ms)", 0.05, 200.0, HINT_DEFAULT_LOW | HINT_LOG, 5.0),
        ctl("Release (ms)", 1.0, 2000.0, HINT_DEFAULT_LOW | HINT_LOG, 120.0),
        ctl("Makeup (dB)", 0.0, 30.0, HINT_DEFAULT_0, 0.0),
        ctl("RMS (ms)", 0.0, 300.0, HINT_DEFAULT_0, 0.0),
    ];
    fn new(rate: f32) -> Self {
        Compressor::new(rate)
    }
    fn reset(&mut self) {
        Compressor::reset(self)
    }
    fn run(&mut self, c: &[f32], input: &[f32], output: &mut [f32]) {
        let p = CompParams {
            threshold_db: c[0],
            ratio: c[1],
            knee_db: c[2],
            attack_ms: c[3],
            release_ms: c[4],
            makeup_db: c[5],
            rms_ms: c[6],
        };
        self.process(&p, input, output);
    }
}

impl Plugin for Deesser {
    const ID: c_ulong = 0x4A7C_0003;
    const LABEL: &'static str = "arctis_deesser";
    const NAME: &'static str = "Arctis De-esser";
    const CONTROLS: &'static [Control] = &[
        ctl("Frequency (Hz)", 2000.0, 16000.0, HINT_DEFAULT_LOW | HINT_LOG, 6000.0),
        ctl("Threshold (dB)", -60.0, 0.0, HINT_DEFAULT_MID, -30.0),
        ctl("Amount (dB)", 0.0, 24.0, HINT_DEFAULT_0, 0.0),
    ];
    fn new(rate: f32) -> Self {
        Deesser::new(rate)
    }
    fn reset(&mut self) {
        Deesser::reset(self)
    }
    fn run(&mut self, c: &[f32], input: &[f32], output: &mut [f32]) {
        let p = DeesserParams {
            freq_hz: c[0],
            threshold_db: c[1],
            amount_db: c[2],
        };
        self.process(&p, input, output);
    }
}

impl Plugin for Limiter {
    const ID: c_ulong = 0x4A7C_0004;
    const LABEL: &'static str = "arctis_limiter";
    const NAME: &'static str = "Arctis Limiter";
    const CONTROLS: &'static [Control] = &[
        ctl("Input Gain (dB)", -24.0, 24.0, HINT_DEFAULT_0, 0.0),
        ctl("Ceiling (dB)", -24.0, 0.0, HINT_DEFAULT_MAX, -1.0),
        ctl("Release (ms)", 1.0, 1000.0, HINT_DEFAULT_LOW | HINT_LOG, 80.0),
    ];
    fn new(rate: f32) -> Self {
        Limiter::new(rate)
    }
    fn reset(&mut self) {
        Limiter::reset(self)
    }
    fn run(&mut self, c: &[f32], input: &[f32], output: &mut [f32]) {
        let p = LimiterParams {
            input_db: c[0],
            ceiling_db: c[1],
            release_ms: c[2],
        };
        self.process(&p, input, output);
    }
}

impl Plugin for Warmth {
    const ID: c_ulong = 0x4A7C_0005;
    const LABEL: &'static str = "arctis_warmth";
    const NAME: &'static str = "Arctis Warmth";
    const CONTROLS: &'static [Control] = &[
        ctl("Drive (dB)", 0.0, 30.0, HINT_DEFAULT_0, 0.0),
        ctl("Body (dB)", -60.0, 12.0, HINT_DEFAULT_MIN, -60.0),
        ctl("Body Freq (Hz)", 60.0, 400.0, HINT_DEFAULT_LOW | HINT_LOG, 180.0),
    ];
    fn new(rate: f32) -> Self {
        Warmth::new(rate)
    }
    fn reset(&mut self) {
        Warmth::reset(self)
    }
    fn run(&mut self, c: &[f32], input: &[f32], output: &mut [f32]) {
        let p = WarmthParams {
            drive_db: c[0],
            body_db: c[1],
            body_hz: c[2],
        };
        self.process(&p, input, output);
    }
}

impl Plugin for Agc {
    const ID: c_ulong = 0x4A7C_0006;
    const LABEL: &'static str = "arctis_agc";
    const NAME: &'static str = "Arctis Leveler";
    const CONTROLS: &'static [Control] = &[
        ctl("Target (dB)", -40.0, -6.0, HINT_DEFAULT_MID, -20.0),
        ctl("Max Boost (dB)", 0.0, 30.0, HINT_DEFAULT_MID, 18.0),
        ctl("Max Cut (dB)", 0.0, 30.0, HINT_DEFAULT_LOW, 12.0),
        ctl("Floor (dB)", -90.0, -20.0, HINT_DEFAULT_MID, -50.0),
        ctl("Amount", 0.0, 1.0, HINT_DEFAULT_0, 0.0),
    ];
    fn new(rate: f32) -> Self {
        Agc::new(rate)
    }
    fn reset(&mut self) {
        Agc::reset(self)
    }
    fn run(&mut self, c: &[f32], input: &[f32], output: &mut [f32]) {
        let p = AgcParams {
            target_db: c[0],
            max_boost_db: c[1],
            max_cut_db: c[2],
            floor_db: c[3],
            amount: c[4],
        };
        self.process(&p, input, output);
    }
}

impl Plugin for Breath {
    const ID: c_ulong = 0x4A7C_0007;
    const LABEL: &'static str = "arctis_breath";
    const NAME: &'static str = "Arctis Breath Reducer";
    const CONTROLS: &'static [Control] = &[
        ctl("Amount (dB)", 0.0, 40.0, HINT_DEFAULT_0, 0.0),
        ctl("Sensitivity", 0.0, 1.0, HINT_DEFAULT_MID, 0.5),
    ];
    fn new(rate: f32) -> Self {
        Breath::new(rate)
    }
    fn reset(&mut self) {
        Breath::reset(self)
    }
    fn run(&mut self, c: &[f32], input: &[f32], output: &mut [f32]) {
        let p = BreathParams {
            amount_db: c[0],
            sensitivity: c[1],
        };
        self.process(&p, input, output);
    }
}

fn table() -> &'static [Built; 7] {
    static TABLE: OnceLock<[Built; 7]> = OnceLock::new();
    TABLE.get_or_init(|| {
        [
            build::<Gate>(),
            build::<Compressor>(),
            build::<Deesser>(),
            build::<Limiter>(),
            build::<Warmth>(),
            build::<Agc>(),
            build::<Breath>(),
        ]
    })
}

/// LADSPA entry point.
///
/// # Safety
/// Called by LADSPA hosts; returns pointers to process-lifetime statics.
#[no_mangle]
pub extern "C" fn ladspa_descriptor(index: c_ulong) -> *const Descriptor {
    table()
        .get(index as usize)
        .map(|b| &b.descriptor as *const Descriptor)
        .unwrap_or(std::ptr::null())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn exports_seven_plugins_with_named_ports() {
        let mut labels = Vec::new();
        for i in 0..7 {
            let d = unsafe { &*ladspa_descriptor(i) };
            labels.push(unsafe { CStr::from_ptr(d.label) }.to_str().unwrap().to_string());
            let first = unsafe { CStr::from_ptr(*d.port_names) };
            assert_eq!(first.to_str().unwrap(), "In");
        }
        assert!(ladspa_descriptor(7).is_null());
        assert_eq!(
            labels,
            [
                "arctis_gate",
                "arctis_comp",
                "arctis_deesser",
                "arctis_limiter",
                "arctis_warmth",
                "arctis_agc",
                "arctis_breath"
            ]
        );
    }

    #[test]
    fn run_through_abi_processes_in_place() {
        let d = unsafe { &*ladspa_descriptor(1) };
        let h = (d.instantiate)(d, 48_000);
        let mut buf: Vec<f32> = (0..256).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        let orig = buf.clone();
        let mut controls = [-18.0f32, 1.0, 6.0, 5.0, 120.0, 6.0, 0.0];
        (d.connect_port)(h, 0, buf.as_mut_ptr());
        (d.connect_port)(h, 1, buf.as_mut_ptr());
        for (i, c) in controls.iter_mut().enumerate() {
            (d.connect_port)(h, 2 + i as c_ulong, c);
        }
        (d.activate.unwrap())(h);
        (d.run)(h, 256);
        (d.cleanup)(h);
        // ratio 1 => pure +6 dB makeup.
        assert!((buf[10] - orig[10] * 1.9953).abs() < 1e-3);
    }
}
