//! Sample-by-sample dynamics processors. Pure Rust, no I/O, no allocation
//! after construction — the LADSPA glue in `ladspa.rs` feeds them blocks.

const EPS: f32 = 1e-9;

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    20.0 * (lin.abs() + EPS).log10()
}

/// One-pole smoothing coefficient for a time constant in milliseconds.
#[inline]
fn coef(ms: f32, rate: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * rate)).exp()
    }
}

// ---------------------------------------------------------------- gate ---

#[derive(Clone, Copy, Debug)]
pub struct GateParams {
    /// Opens above this level.
    pub threshold_db: f32,
    /// Attenuation while closed; 0 dB disables the gate.
    pub range_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            threshold_db: -50.0,
            range_db: 0.0,
            attack_ms: 1.0,
            hold_ms: 150.0,
            release_ms: 180.0,
        }
    }
}

/// Peak-detecting gate with 4 dB hysteresis and a hold timer, so syllable
/// tails are not chopped.
pub struct Gate {
    rate: f32,
    env: f32,
    gain: f32,
    hold_left: u32,
    open: bool,
}

impl Gate {
    pub fn new(rate: f32) -> Self {
        Self {
            rate,
            env: 0.0,
            gain: 1.0,
            hold_left: 0,
            open: true,
        }
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.gain = 1.0;
        self.hold_left = 0;
        self.open = true;
    }

    pub fn process(&mut self, p: &GateParams, input: &[f32], output: &mut [f32]) {
        let floor = db_to_lin(p.range_db.min(0.0));
        if p.range_db >= -0.01 {
            output.copy_from_slice(input);
            self.gain = 1.0;
            return;
        }
        let open_at = db_to_lin(p.threshold_db);
        let close_at = db_to_lin(p.threshold_db - 4.0);
        let env_rel = coef(25.0, self.rate);
        let att = coef(p.attack_ms.max(0.05), self.rate);
        let rel = coef(p.release_ms.max(1.0), self.rate);
        let hold = (p.hold_ms.max(0.0) * 0.001 * self.rate) as u32;

        for (x, y) in input.iter().zip(output.iter_mut()) {
            let a = x.abs();
            self.env = if a > self.env { a } else { a + env_rel * (self.env - a) };
            if self.env >= open_at {
                self.open = true;
                self.hold_left = hold;
            } else if self.env < close_at {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }
            let target = if self.open { 1.0 } else { floor };
            let c = if target > self.gain { att } else { rel };
            self.gain = target + c * (self.gain - target);
            *y = x * self.gain;
        }
    }
}

// ---------------------------------------------------------- compressor ---

#[derive(Clone, Copy, Debug)]
pub struct CompParams {
    pub threshold_db: f32,
    /// 1 = off.
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    /// RMS detector window; 0 = peak detection. RMS reacts to loudness the
    /// way ears do, which is what gives levelled voice its smooth density.
    pub rms_ms: f32,
}

impl Default for CompParams {
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 1.0,
            knee_db: 6.0,
            attack_ms: 5.0,
            release_ms: 120.0,
            makeup_db: 0.0,
            rms_ms: 0.0,
        }
    }
}

/// Static gain computer: gain reduction in dB (<= 0) for an input level,
/// with a quadratic soft knee (Giannoulis, Massberg & Reiss 2012).
pub fn comp_gain_db(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let ratio = ratio.max(1.0);
    let slope = 1.0 / ratio - 1.0;
    let over = level_db - threshold;
    if knee > 0.0 && 2.0 * over.abs() <= knee {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

/// Log-domain feed-forward compressor with smooth branching attack/release.
pub struct Compressor {
    rate: f32,
    gr_db: f32,
    power: f32,
    pub last_gr_db: f32,
}

impl Compressor {
    pub fn new(rate: f32) -> Self {
        Self {
            rate,
            gr_db: 0.0,
            power: 0.0,
            last_gr_db: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.gr_db = 0.0;
        self.power = 0.0;
        self.last_gr_db = 0.0;
    }

    pub fn process(&mut self, p: &CompParams, input: &[f32], output: &mut [f32]) {
        let makeup = db_to_lin(p.makeup_db);
        if p.ratio <= 1.001 {
            for (x, y) in input.iter().zip(output.iter_mut()) {
                *y = x * makeup;
            }
            self.gr_db = 0.0;
            self.last_gr_db = 0.0;
            return;
        }
        let att = coef(p.attack_ms.max(0.05), self.rate);
        let rel = coef(p.release_ms.max(1.0), self.rate);
        let rms = coef(p.rms_ms, self.rate);
        let mut deepest = 0.0f32;
        for (x, y) in input.iter().zip(output.iter_mut()) {
            let level_db = if p.rms_ms > 0.0 {
                self.power = x * x + rms * (self.power - x * x);
                // +3 dB so a sine reads the same as with peak detection.
                10.0 * (self.power + 1e-12).log10() + 3.01
            } else {
                lin_to_db(*x)
            };
            let target = comp_gain_db(level_db, p.threshold_db, p.ratio, p.knee_db);
            // gr is negative: moving more negative is the attack phase.
            let c = if target < self.gr_db { att } else { rel };
            self.gr_db = target + c * (self.gr_db - target);
            deepest = deepest.min(self.gr_db);
            *y = x * db_to_lin(self.gr_db) * makeup;
        }
        self.last_gr_db = deepest;
    }
}

// ------------------------------------------------------------ biquad ---

/// RBJ cookbook 2nd-order Butterworth low-pass, transposed direct form II.
#[derive(Clone, Copy, Default)]
struct LowPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl LowPass {
    fn design(&mut self, freq: f32, rate: f32) {
        let w0 = 2.0 * std::f32::consts::PI * (freq / rate).clamp(1e-4, 0.49);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        self.b0 = (1.0 - cos) / 2.0 / a0;
        self.b1 = (1.0 - cos) / a0;
        self.b2 = (1.0 - cos) / 2.0 / a0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

// ------------------------------------------------------------ breath ---

#[derive(Clone, Copy, Debug)]
pub struct BreathParams {
    /// How far detected breaths are turned down; 0 = off (bit-exact).
    pub amount_db: f32,
    /// 0..1: higher treats more borderline sounds as breath.
    pub sensitivity: f32,
}

impl Default for BreathParams {
    fn default() -> Self {
        Self {
            amount_db: 0.0,
            sensitivity: 0.5,
        }
    }
}

const BREATH_HOP_MS: f32 = 5.0;
const BREATH_LOOKAHEAD_MS: f32 = 30.0;
const ANALYSIS_RATE: f32 = 8000.0;
const WIN: usize = 240; // 30 ms at 8 kHz
const MIN_LAG: usize = 20; // 400 Hz
const MAX_LAG: usize = 115; // ~70 Hz
const MAX_DELAY: usize = 4096;

/// Breath reducer. A breath is *unvoiced* (no pitch), clearly quieter than
/// the speech around it, and not a sibilant. Every 5 ms the detector checks
/// voicing with a normalised autocorrelation on a decimated low band, the
/// level against a tracker of recent voiced speech, and the share of energy
/// above 5 kHz. Matching sound is ducked; the audio runs 30 ms behind the
/// detector, so the duck lifts *before* the next word starts.
pub struct Breath {
    rate: f32,
    delay: Box<[f32; MAX_DELAY]>,
    delay_len: usize,
    dpos: usize,
    lp: LowPass,
    lm_split: LowPass,
    decim: usize,
    decim_count: usize,
    win: [f32; WIN],
    wpos: usize,
    hop: usize,
    hop_count: usize,
    power: f32,
    lm_power: f32,
    peak_db: f32,
    run: u32,
    voiced_run: u32,
    breath: bool,
    /// Running level of the current breath, to spot a consonant starting.
    breath_db: f32,
    /// Cleared when a consonant onset ends a breath, so the consonant itself
    /// is not taken for a new breath; re-armed by voiced speech.
    armed: bool,
    gain_db: f32,
    pub last_voicing: f32,
}

impl Breath {
    pub fn new(rate: f32) -> Self {
        let mut lp = LowPass::default();
        lp.design(1000.0, rate);
        let mut lm_split = LowPass::default();
        lm_split.design(2500.0, rate);
        let delay_len = ((BREATH_LOOKAHEAD_MS * 0.001 * rate) as usize).clamp(1, MAX_DELAY);
        Self {
            rate,
            delay: Box::new([0.0; MAX_DELAY]),
            delay_len,
            dpos: 0,
            lp,
            lm_split,
            decim: ((rate / ANALYSIS_RATE).round() as usize).max(1),
            decim_count: 0,
            win: [0.0; WIN],
            wpos: 0,
            hop: ((BREATH_HOP_MS * 0.001 * rate) as usize).max(1),
            hop_count: 0,
            power: 0.0,
            lm_power: 0.0,
            peak_db: -30.0,
            run: 0,
            voiced_run: 0,
            breath: false,
            breath_db: -90.0,
            armed: true,
            gain_db: 0.0,
            last_voicing: 0.0,
        }
    }

    pub fn latency(&self) -> usize {
        self.delay_len
    }


    pub fn reset(&mut self) {
        *self = Self::new(self.rate);
    }

    /// Best normalised autocorrelation over the voice pitch range, after
    /// centre clipping (Sondhi): keeping only the top of each waveform peak
    /// leaves the pitch pulses of a voice but makes noise far less
    /// self-similar.
    fn voicing(&self) -> f32 {
        // Unroll the ring into chronological order.
        let mut w = [0.0f32; WIN];
        for (i, v) in w.iter_mut().enumerate() {
            *v = self.win[(self.wpos + i) % WIN];
        }
        let clip = 0.3 * w.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        for v in w.iter_mut() {
            *v = if *v > clip {
                *v - clip
            } else if *v < -clip {
                *v + clip
            } else {
                0.0
            };
        }
        let mut best = 0.0f32;
        for lag in MIN_LAG..=MAX_LAG {
            let (mut xy, mut xx, mut yy) = (0.0f32, 0.0f32, 0.0f32);
            for n in 0..WIN - lag {
                let (a, b) = (w[n], w[n + lag]);
                xy += a * b;
                xx += a * a;
                yy += b * b;
            }
            if xx > 1e-12 && yy > 1e-12 {
                best = best.max(xy / (xx * yy).sqrt());
            }
        }
        best
    }

    fn decide(&mut self, p: &BreathParams) {
        let sens = p.sensitivity.clamp(0.0, 1.0);
        let level = 10.0 * (self.power + 1e-12).log10();
        // Share of energy below 2.5 kHz: breaths have plenty, "s"/"sh" almost none.
        let lm_ratio = self.lm_power / (self.power + 1e-12);
        let voicing = self.voicing();
        self.last_voicing = voicing;
        let voiced = voicing > 0.35 + 0.3 * sens;

        if voiced && level > -70.0 {
            self.peak_db = self.peak_db.max(level);
        } else {
            // Forget the last loud passage slowly (1 dB/s).
            self.peak_db = (self.peak_db - BREATH_HOP_MS * 0.001).max(-60.0);
        }
        let margin = 6.0 + 10.0 * (1.0 - sens);
        let quiet = level < self.peak_db - margin;
        let sibilant = lm_ratio < 0.2;
        let candidate = !voiced && quiet && !sibilant && level > -80.0;
        self.run = if candidate { self.run + 1 } else { 0 };
        self.voiced_run = if voiced { self.voiced_run + 1 } else { 0 };
        if voiced {
            self.armed = true;
        }
        if self.breath {
            // Leave on real voicing, a sibilant, or a jump in level (a word
            // starting with "f"/"h"/"th" is unvoiced too, but it is louder
            // than the breath before it) — not on one noisy frame.
            let onset = level > self.breath_db + 5.0;
            if self.voiced_run >= 2 || !quiet || sibilant || onset {
                self.breath = false;
                self.run = 0;
                if onset {
                    self.armed = false;
                }
            } else {
                self.breath_db = level + 0.8 * (self.breath_db - level);
            }
        } else {
            // 60 ms of breath-like sound: real breaths last 200-600 ms, while
            // "f"/"h" inside a word are usually shorter.
            // A disarmed detector still catches a long (300 ms) breath.
            self.breath = self.run >= 12 && (self.armed || self.run >= 60);
            self.breath_db = level;
        }
    }

    pub fn process(&mut self, p: &BreathParams, input: &[f32], output: &mut [f32]) {
        if p.amount_db <= 0.01 {
            output.copy_from_slice(input);
            self.gain_db = 0.0;
            return;
        }
        let pc = coef(15.0, self.rate);
        let down = coef(25.0, self.rate);
        let up = coef(3.0, self.rate);
        let floor = -p.amount_db.abs().min(40.0);
        for (x, y) in input.iter().zip(output.iter_mut()) {
            let v = *x;
            self.power = v * v + pc * (self.power - v * v);
            let lm = self.lm_split.tick(v);
            self.lm_power = lm * lm + pc * (self.lm_power - lm * lm);

            let low = self.lp.tick(v);
            self.decim_count += 1;
            if self.decim_count >= self.decim {
                self.decim_count = 0;
                self.win[self.wpos] = low;
                self.wpos = (self.wpos + 1) % WIN;
            }
            self.hop_count += 1;
            if self.hop_count >= self.hop {
                self.hop_count = 0;
                self.decide(p);
            }

            let target = if self.breath { floor } else { 0.0 };
            let c = if target < self.gain_db { down } else { up };
            self.gain_db = target + c * (self.gain_db - target);

            let delayed = self.delay[self.dpos];
            self.delay[self.dpos] = v;
            self.dpos = (self.dpos + 1) % self.delay_len;
            *y = delayed * db_to_lin(self.gain_db);
        }
    }
}

// --------------------------------------------------------------- agc ---

#[derive(Clone, Copy, Debug)]
pub struct AgcParams {
    /// Speech level (RMS) the leveler steers towards.
    pub target_db: f32,
    pub max_boost_db: f32,
    pub max_cut_db: f32,
    /// Below this level the gain is frozen, so pauses and room noise are
    /// never pumped up.
    pub floor_db: f32,
    /// 0 = off (bit-exact), 1 = full correction.
    pub amount: f32,
}

impl Default for AgcParams {
    fn default() -> Self {
        Self {
            target_db: -20.0,
            max_boost_db: 18.0,
            max_cut_db: 12.0,
            floor_db: -50.0,
            amount: 0.0,
        }
    }
}

/// Slow automatic gain control — the "hand on the fader" of a broadcast
/// chain. It brings speech to a fixed level before the compressor, so the
/// compressor works the same whether you lean into the mic or not.
pub struct Agc {
    rate: f32,
    power: f32,
    gain_db: f32,
    /// Recent speech level; the gain only adapts within 9 dB of it, so
    /// breaths and pauses between words are never ridden up.
    speech_db: f32,
}

impl Agc {
    pub fn new(rate: f32) -> Self {
        Self {
            rate,
            power: 0.0,
            gain_db: 0.0,
            speech_db: -90.0,
        }
    }

    pub fn reset(&mut self) {
        self.power = 0.0;
        self.gain_db = 0.0;
        self.speech_db = -90.0;
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    pub fn process(&mut self, p: &AgcParams, input: &[f32], output: &mut [f32]) {
        let amount = p.amount.clamp(0.0, 1.0);
        if amount <= 0.001 {
            output.copy_from_slice(input);
            self.gain_db = 0.0;
            return;
        }
        let detect = coef(120.0, self.rate);
        // Turning down is quicker than turning up, like a careful engineer.
        let down = coef(250.0, self.rate);
        let up = coef(1200.0, self.rate);
        // The speech-level tracker forgets at 1.5 dB per second.
        let forget = 1.5 / self.rate;
        for (x, y) in input.iter().zip(output.iter_mut()) {
            self.power = x * x + detect * (self.power - x * x);
            let level = 10.0 * (self.power + 1e-12).log10() + 3.01;
            self.speech_db = if level > self.speech_db {
                level
            } else {
                self.speech_db - forget
            };
            if level > p.floor_db && level > self.speech_db - 9.0 {
                let want = ((p.target_db - level) * amount)
                    .clamp(-p.max_cut_db.abs(), p.max_boost_db.abs());
                let c = if want < self.gain_db { down } else { up };
                self.gain_db = want + c * (self.gain_db - want);
            }
            *y = x * db_to_lin(self.gain_db);
        }
    }
}

// ------------------------------------------------------------ warmth ---

#[derive(Clone, Copy, Debug)]
pub struct WarmthParams {
    /// Tube-style saturation drive; 0 = clean.
    pub drive_db: f32,
    /// Level of synthesised low harmonics ("bass body"); 0 = off.
    pub body_db: f32,
    /// Upper edge of the band the body harmonics are generated from.
    pub body_hz: f32,
}

impl Default for WarmthParams {
    fn default() -> Self {
        Self {
            drive_db: 0.0,
            body_db: -60.0,
            body_hz: 180.0,
        }
    }
}

/// Two kinds of "richness" a small headset capsule cannot capture:
///
/// * **Tube saturation** — an asymmetric soft clipper, gain-normalised so
///   quiet signals pass at unity. The asymmetry adds even harmonics (warm,
///   not fuzzy); a DC blocker removes the offset it creates.
/// * **Bass body** — psychoacoustic bass: the low band is full-wave rectified
///   (which produces its 2nd harmonic), DC-blocked and low-passed, then mixed
///   in. The ear hears the fundamental from its harmonics, so the voice
///   sounds deeper without pushing sub-bass the mic never picked up.
///
/// With drive 0 and body at -60 dB or below the output is bit-exact.
pub struct Warmth {
    rate: f32,
    split: LowPass,
    smooth: LowPass,
    designed_for: f32,
    dc_body: f32,
    dc_body_out: f32,
    dc_sat: f32,
    dc_sat_out: f32,
}

const SAT_BIAS: f32 = 0.18;

impl Warmth {
    pub fn new(rate: f32) -> Self {
        Self {
            rate,
            split: LowPass::default(),
            smooth: LowPass::default(),
            designed_for: -1.0,
            dc_body: 0.0,
            dc_body_out: 0.0,
            dc_sat: 0.0,
            dc_sat_out: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.split.reset();
        self.smooth.reset();
        self.dc_body = 0.0;
        self.dc_body_out = 0.0;
        self.dc_sat = 0.0;
        self.dc_sat_out = 0.0;
    }

    pub fn process(&mut self, p: &WarmthParams, input: &[f32], output: &mut [f32]) {
        let body_on = p.body_db > -59.9;
        let drive_on = p.drive_db > 0.01;
        if !body_on && !drive_on {
            output.copy_from_slice(input);
            return;
        }
        let freq = p.body_hz.clamp(60.0, 400.0);
        if (freq - self.designed_for).abs() > 0.5 {
            self.split.design(freq, self.rate);
            self.smooth.design((freq * 2.5).min(self.rate * 0.45), self.rate);
            self.designed_for = freq;
        }
        let body = db_to_lin(p.body_db);
        let g = db_to_lin(p.drive_db.clamp(0.0, 30.0));
        let t0 = SAT_BIAS.tanh();
        // Small-signal slope of tanh(g·x + b) at 0 is g·(1 - tanh²b).
        let norm = 1.0 / (g * (1.0 - t0 * t0));
        // One-pole DC blockers at ~10 Hz.
        let r = 1.0 - (2.0 * std::f32::consts::PI * 10.0 / self.rate);

        for (x, y) in input.iter().zip(output.iter_mut()) {
            let mut v = *x;
            if body_on {
                let low = self.split.tick(v);
                let rect = low.abs();
                let hp = rect - self.dc_body + r * self.dc_body_out;
                self.dc_body = rect;
                self.dc_body_out = hp;
                v += body * self.smooth.tick(hp);
            }
            if drive_on {
                let sat = ((g * v + SAT_BIAS).tanh() - t0) * norm;
                let hp = sat - self.dc_sat + r * self.dc_sat_out;
                self.dc_sat = sat;
                self.dc_sat_out = hp;
                v = hp;
            }
            *y = v;
        }
    }
}

// ----------------------------------------------------------- de-esser ---

#[derive(Clone, Copy, Debug)]
pub struct DeesserParams {
    pub freq_hz: f32,
    pub threshold_db: f32,
    /// Maximum reduction of the sibilant band; 0 disables.
    pub amount_db: f32,
}

impl Default for DeesserParams {
    fn default() -> Self {
        Self {
            freq_hz: 6000.0,
            threshold_db: -30.0,
            amount_db: 0.0,
        }
    }
}

/// Complementary split at `freq_hz`: `high = x - lowpass(x)`, so
/// `low + high == x` exactly. Only the high band is turned down while it is
/// louder than the threshold; with no reduction the output is bit-exact.
pub struct Deesser {
    rate: f32,
    lp: LowPass,
    designed_for: f32,
    env: f32,
    gain: f32,
}

impl Deesser {
    pub fn new(rate: f32) -> Self {
        Self {
            rate,
            lp: LowPass::default(),
            designed_for: -1.0,
            env: 0.0,
            gain: 1.0,
        }
    }

    pub fn reset(&mut self) {
        self.lp.reset();
        self.env = 0.0;
        self.gain = 1.0;
    }

    pub fn process(&mut self, p: &DeesserParams, input: &[f32], output: &mut [f32]) {
        if p.amount_db <= 0.01 {
            output.copy_from_slice(input);
            return;
        }
        let freq = p.freq_hz.clamp(1500.0, self.rate * 0.45);
        if (freq - self.designed_for).abs() > 0.5 {
            self.lp.design(freq, self.rate);
            self.designed_for = freq;
        }
        let env_att = coef(0.5, self.rate);
        let env_rel = coef(40.0, self.rate);
        let g_att = coef(1.0, self.rate);
        let g_rel = coef(60.0, self.rate);
        let floor_db = -p.amount_db.abs();
        for (x, y) in input.iter().zip(output.iter_mut()) {
            let high = x - self.lp.tick(*x);
            let a = high.abs();
            let c = if a > self.env { env_att } else { env_rel };
            self.env = a + c * (self.env - a);
            let over = lin_to_db(self.env) - p.threshold_db;
            // 4:1 above the threshold, never deeper than the amount.
            let target = db_to_lin((-(over.max(0.0)) * 0.75).max(floor_db));
            let c = if target < self.gain { g_att } else { g_rel };
            self.gain = target + c * (self.gain - target);
            *y = if self.gain >= 0.99999 { *x } else { x - (1.0 - self.gain) * high };
        }
    }
}

// ------------------------------------------------------------ limiter ---

#[derive(Clone, Copy, Debug)]
pub struct LimiterParams {
    pub input_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
}

impl Default for LimiterParams {
    fn default() -> Self {
        Self {
            input_db: 0.0,
            ceiling_db: -1.0,
            release_ms: 80.0,
        }
    }
}

pub const LOOKAHEAD_MS: f32 = 1.5;
const MAX_LOOKAHEAD: usize = 1024;

/// Look-ahead limiter: sliding minimum of the required gain over the window,
/// release-limited, then box-averaged over the same window. The average of
/// values that are all <= the requirement is <= the requirement, so the
/// delayed peak never exceeds the ceiling, and the ramp has no steps.
pub struct Limiter {
    len: usize,
    delay: [f32; MAX_LOOKAHEAD],
    req: [f32; MAX_LOOKAHEAD],
    smooth: [f32; MAX_LOOKAHEAD],
    pos: usize,
    held: f32,
    sum: f64,
    rate: f32,
}

impl Limiter {
    pub fn new(rate: f32) -> Self {
        let len = ((LOOKAHEAD_MS * 0.001 * rate) as usize).clamp(1, MAX_LOOKAHEAD);
        Self {
            len,
            delay: [0.0; MAX_LOOKAHEAD],
            req: [1.0; MAX_LOOKAHEAD],
            smooth: [1.0; MAX_LOOKAHEAD],
            pos: 0,
            held: 1.0,
            sum: len as f64,
            rate,
        }
    }

    /// Delay introduced, in samples.
    pub fn latency(&self) -> usize {
        self.len - 1
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.rate);
    }

    pub fn process(&mut self, p: &LimiterParams, input: &[f32], output: &mut [f32]) {
        let pre = db_to_lin(p.input_db);
        let ceiling = db_to_lin(p.ceiling_db.min(0.0));
        let rel = 1.0 - coef(p.release_ms.max(1.0), self.rate);
        let n = self.len;
        for (x, y) in input.iter().zip(output.iter_mut()) {
            let s = x * pre;
            let need = if s.abs() > ceiling { ceiling / s.abs() } else { 1.0 };

            // Ring of the last `n` inputs and requirements.
            let oldest = self.pos;
            let delayed = self.delay[(oldest + 1) % n];
            self.delay[oldest] = s;
            self.req[oldest] = need;

            // Sliding minimum over the window (n is small: <= 72 at 48 kHz).
            let mut min = need;
            for i in 0..n {
                min = min.min(self.req[i]);
            }
            // Recover towards unity no faster than the release time.
            self.held = min.min(self.held + (1.0 - self.held) * rel);

            self.sum += f64::from(self.held) - f64::from(self.smooth[oldest]);
            self.smooth[oldest] = self.held;
            let gain = (self.sum / n as f64) as f32;

            self.pos = (self.pos + 1) % n;
            *y = delayed * gain.min(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / RATE).sin())
            .collect()
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn knee_is_continuous_and_reduces_above_threshold() {
        assert_eq!(comp_gain_db(-40.0, -20.0, 4.0, 6.0), 0.0);
        // 10 dB over at 4:1 -> output 2.5 dB over -> -7.5 dB.
        assert!((comp_gain_db(-10.0, -20.0, 4.0, 6.0) + 7.5).abs() < 1e-4);
        let edge = comp_gain_db(-17.0, -20.0, 4.0, 6.0);
        assert!((edge - -2.25).abs() < 1e-3, "{edge}");
    }

    #[test]
    fn compressor_bypass_is_exact_with_makeup() {
        let mut c = Compressor::new(RATE);
        let x = sine(440.0, 0.5, 512);
        let mut y = vec![0.0; 512];
        c.process(&CompParams::default(), &x, &mut y);
        assert_eq!(x, y);
        let p = CompParams { makeup_db: 6.0206, ..Default::default() };
        c.process(&p, &x, &mut y);
        assert!((y[100] - 2.0 * x[100]).abs() < 1e-4);
    }

    #[test]
    fn compressor_tames_loud_signal() {
        let mut c = Compressor::new(RATE);
        let p = CompParams { threshold_db: -20.0, ratio: 4.0, knee_db: 0.0, ..Default::default() };
        let x = sine(300.0, 1.0, 48_000);
        let mut y = vec![0.0; x.len()];
        c.process(&p, &x, &mut y);
        let tail = peak(&y[24_000..]);
        assert!(tail < 0.5, "peak after attack {tail}");
    }

    /// Deterministic white-ish noise.
    fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                amp * ((s as f32 / u32::MAX as f32) * 2.0 - 1.0)
            })
            .collect()
    }

    /// Voiced-like: 120 Hz with its first harmonics.
    fn voice(n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE;
                (1..12)
                    .map(|h| (2.0 * std::f32::consts::PI * 120.0 * h as f32 * t).sin() / h as f32)
                    .sum::<f32>()
                    * amp
            })
            .collect()
    }

    fn rms_db(x: &[f32]) -> f32 {
        10.0 * (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32 + 1e-12).log10()
    }

    fn lowpass(x: &[f32], f: f32) -> Vec<f32> {
        let mut lp = LowPass::default();
        lp.design(f, RATE);
        x.iter().map(|v| lp.tick(*v)).collect()
    }

    fn highpass(x: &[f32], f: f32) -> Vec<f32> {
        let mut lp = LowPass::default();
        lp.design(f, RATE);
        x.iter().map(|v| v - lp.tick(*v)).collect()
    }

    #[test]
    fn breath_between_words_is_ducked_voice_is_not() {
        let p = BreathParams { amount_db: 15.0, sensitivity: 0.5 };
        // word (0.5 s) - breath (0.4 s) - word (0.5 s)
        let word = voice(24_000, 0.2);
        let breath = lowpass(&noise(19_200, 0.05, 7), 3000.0);
        let mut x = word.clone();
        x.extend(&breath);
        x.extend(&word);
        let mut y = vec![0.0; x.len()];
        let mut b = Breath::new(RATE);
        b.process(&p, &x, &mut y);
        let d = b.latency();
        let seg = |a: usize, z: usize| rms_db(&y[a + d..z + d]);
        let src = |a: usize, z: usize| rms_db(&x[a..z]);
        let ducked = src(28_800, 38_400) - seg(28_800, 38_400);
        assert!(ducked > 10.0, "breath only ducked {ducked} dB");
        let w1 = src(4_800, 19_200) - seg(4_800, 19_200);
        let w2 = src(45_600, 62_400) - seg(45_600, 62_400);
        assert!(w1.abs() < 0.5 && w2.abs() < 0.5, "voice changed {w1} / {w2}");
    }

    #[test]
    fn sibilants_are_not_treated_as_breath() {
        let p = BreathParams { amount_db: 15.0, sensitivity: 0.5 };
        let mut x = voice(24_000, 0.2);
        let s = highpass(&noise(9_600, 0.08, 3), 6000.0);
        x.extend(&s);
        let mut y = vec![0.0; x.len()];
        let mut b = Breath::new(RATE);
        b.process(&p, &x, &mut y);
        let d = b.latency();
        let change = rms_db(&x[25_000..31_000]) - rms_db(&y[25_000 + d..31_000 + d]);
        assert!(change < 1.0, "sibilant ducked {change} dB");
    }

    /// Debug aid: `ARCTIS_WAV=file.wav cargo test --lib breath_wav -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn breath_wav_trace() {
        let Ok(path) = std::env::var("ARCTIS_WAV") else { return };
        let bytes = std::fs::read(path).unwrap();
        let data = &bytes[44..];
        let x: Vec<f32> = data.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0).collect();
        let p = BreathParams { amount_db: 15.0, sensitivity: 0.5 };
        let mut b = Breath::new(RATE);
        let mut out = vec![0.0; 2400];
        for (i, chunk) in x.chunks(2400).enumerate() {
            b.process(&p, chunk, &mut out[..chunk.len()]);
            let lm = b.lm_power / (b.power + 1e-12);
            eprintln!("t={:5}ms breath={} gain={:6.1} voicing={:.2} peak={:6.1} lvl={:6.1} lm={:.2} run={}",
                i * 50, b.breath as u8, b.gain_db, b.last_voicing, b.peak_db,
                10.0 * (b.power + 1e-12).log10(), lm, b.run);
        }
    }

    #[test]
    fn word_starting_with_fricative_after_breath_is_kept() {
        let p = BreathParams { amount_db: 15.0, sensitivity: 0.5 };
        let mut x = voice(24_000, 0.2);
        x.extend(lowpass(&noise(19_200, 0.01, 7), 3000.0)); // breath, ~-45 dB
        let f = lowpass(&noise(4_800, 0.06, 11), 6000.0); // "f", ~-30 dB, 100 ms
        x.extend(&f);
        x.extend(voice(24_000, 0.2));
        let mut y = vec![0.0; x.len()];
        let mut b = Breath::new(RATE);
        b.process(&p, &x, &mut y);
        let d = b.latency();
        let at = 24_000 + 19_200;
        let kept = rms_db(&x[at + 960..at + 4_800]) - rms_db(&y[at + 960 + d..at + 4_800 + d]);
        assert!(kept < 3.0, "fricative ducked {kept} dB");
    }

    #[test]
    fn breath_off_is_exact() {
        let x = noise(2048, 0.1, 9);
        let mut y = vec![0.0; x.len()];
        Breath::new(RATE).process(&BreathParams::default(), &x, &mut y);
        assert_eq!(x, y);
    }

    #[test]
    fn agc_does_not_ride_up_pauses_after_speech() {
        let p = AgcParams { amount: 1.0, ..Default::default() };
        let mut x = sine(200.0, 0.1, 2 * 48_000); // speech at about -20 dB
        x.extend(sine(200.0, 0.01, 2 * 48_000)); // breath 20 dB lower
        let mut y = vec![0.0; x.len()];
        let mut agc = Agc::new(RATE);
        agc.process(&p, &x[..2 * 48_000], &mut y[..2 * 48_000]);
        let before = agc.gain_db();
        agc.process(&p, &x[2 * 48_000..], &mut y[2 * 48_000..]);
        assert!((agc.gain_db() - before).abs() < 1.0, "gain moved {before} -> {}", agc.gain_db());
    }

    #[test]
    fn agc_brings_quiet_and_loud_speech_together() {
        let p = AgcParams { amount: 1.0, ..Default::default() };
        let run = |amp: f32| {
            let x = sine(200.0, amp, 7 * 48_000);
            let mut y = vec![0.0; x.len()];
            Agc::new(RATE).process(&p, &x, &mut y);
            lin_to_db(peak(&y[6 * 48_000..]))
        };
        let (quiet, loud) = (run(0.02), run(0.3));
        // 23.5 dB apart going in; within a few dB coming out.
        assert!((quiet - loud).abs() < 4.0, "{quiet} vs {loud}");
        assert!((quiet - -20.0).abs() < 2.0, "quiet ended at {quiet}");
    }

    #[test]
    fn agc_freezes_below_floor_and_is_exact_when_off() {
        let p = AgcParams { amount: 1.0, ..Default::default() };
        let noise = sine(200.0, 0.0005, 2 * 48_000); // about -69 dBFS
        let mut y = vec![0.0; noise.len()];
        let mut agc = Agc::new(RATE);
        agc.process(&p, &noise, &mut y);
        assert_eq!(agc.gain_db(), 0.0);
        Agc::new(RATE).process(&AgcParams::default(), &noise, &mut y);
        assert_eq!(noise, y);
    }

    #[test]
    fn rms_detector_levels_like_peak_for_sine() {
        let x = sine(300.0, 1.0, 48_000);
        let mut y_peak = vec![0.0; x.len()];
        let mut y_rms = vec![0.0; x.len()];
        let p = CompParams { threshold_db: -20.0, ratio: 4.0, knee_db: 0.0, ..Default::default() };
        Compressor::new(RATE).process(&p, &x, &mut y_peak);
        Compressor::new(RATE).process(&CompParams { rms_ms: 20.0, ..p }, &x, &mut y_rms);
        let (a, b) = (peak(&y_peak[24_000..]), peak(&y_rms[24_000..]));
        assert!((lin_to_db(a) - lin_to_db(b)).abs() < 1.5, "{a} vs {b}");
    }

    #[test]
    fn warmth_neutral_is_exact() {
        let x = sine(200.0, 0.4, 1024);
        let mut y = vec![0.0; x.len()];
        Warmth::new(RATE).process(&WarmthParams::default(), &x, &mut y);
        assert_eq!(x, y);
    }

    #[test]
    fn saturation_keeps_quiet_level_and_adds_harmonics() {
        let p = WarmthParams { drive_db: 12.0, ..Default::default() };
        let quiet = sine(500.0, 0.001, 48_000);
        let mut y = vec![0.0; quiet.len()];
        Warmth::new(RATE).process(&p, &quiet, &mut y);
        let ratio = peak(&y[24_000..]) / 0.001;
        assert!((0.9..1.1).contains(&ratio), "quiet gain {ratio}");

        let loud = sine(500.0, 0.5, 48_000);
        let mut y = vec![0.0; loud.len()];
        Warmth::new(RATE).process(&p, &loud, &mut y);
        // Energy at 2·f (even harmonic from the asymmetry).
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (i, v) in y[24_000..].iter().enumerate() {
            let ph = 2.0 * std::f32::consts::PI * 1000.0 * (i + 24_000) as f32 / RATE;
            re += v * ph.cos();
            im += v * ph.sin();
        }
        let h2 = 2.0 * (re * re + im * im).sqrt() / 24_000.0;
        assert!(h2 > 1e-3, "2nd harmonic {h2}");
        assert!(peak(&y[24_000..]) < 1.0);
    }

    #[test]
    fn body_adds_low_harmonics_only_for_low_input() {
        let p = WarmthParams { body_db: 0.0, body_hz: 180.0, ..Default::default() };
        let low = sine(100.0, 0.3, 48_000);
        let mut y = vec![0.0; low.len()];
        Warmth::new(RATE).process(&p, &low, &mut y);
        let added: f32 = y[24_000..].iter().zip(&low[24_000..]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(added > 0.05, "body added {added}");

        let high = sine(3000.0, 0.3, 48_000);
        let mut y = vec![0.0; high.len()];
        Warmth::new(RATE).process(&p, &high, &mut y);
        let added: f32 = y[24_000..].iter().zip(&high[24_000..]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(added < 0.01, "body leaked {added}");
    }

    #[test]
    fn gate_attenuates_quiet_and_passes_loud() {
        let p = GateParams { threshold_db: -40.0, range_db: -40.0, hold_ms: 0.0, ..Default::default() };
        let mut g = Gate::new(RATE);
        let quiet = sine(200.0, 0.001, 96_000);
        let mut y = vec![0.0; quiet.len()];
        g.process(&p, &quiet, &mut y);
        assert!(peak(&y[90_000..]) < 0.001 * 0.02);
        let loud = sine(200.0, 0.5, 24_000);
        let mut y = vec![0.0; loud.len()];
        g.process(&p, &loud, &mut y);
        assert!(peak(&y[12_000..]) > 0.49);
    }

    #[test]
    fn gate_off_is_exact() {
        let mut g = Gate::new(RATE);
        let x = sine(200.0, 0.001, 256);
        let mut y = vec![0.0; 256];
        g.process(&GateParams::default(), &x, &mut y);
        assert_eq!(x, y);
    }

    #[test]
    fn deesser_cuts_highs_not_lows() {
        let p = DeesserParams { freq_hz: 5000.0, threshold_db: -40.0, amount_db: 12.0 };
        let mut d = Deesser::new(RATE);
        let hiss = sine(8000.0, 0.5, 24_000);
        let mut y = vec![0.0; hiss.len()];
        d.process(&p, &hiss, &mut y);
        assert!(peak(&y[12_000..]) < 0.5 * 0.4);

        let mut d = Deesser::new(RATE);
        let voice = sine(200.0, 0.5, 24_000);
        d.process(&p, &voice, &mut y);
        assert!(peak(&y[12_000..]) > 0.49);
    }

    #[test]
    fn limiter_holds_ceiling_on_transient() {
        let p = LimiterParams { input_db: 12.0, ceiling_db: -1.0, release_ms: 50.0 };
        let mut l = Limiter::new(RATE);
        let mut x = vec![0.0f32; 4800];
        x[1000] = 1.0;
        x[1001] = -0.9;
        x.extend(sine(1000.0, 0.9, 9600));
        let mut y = vec![0.0; x.len()];
        l.process(&p, &x, &mut y);
        let ceiling = db_to_lin(-1.0);
        assert!(peak(&y) <= ceiling + 1e-4, "peak {} > {}", peak(&y), ceiling);
        // The loud tone is still there, just limited.
        assert!(peak(&y[8000..]) > ceiling * 0.9);
    }

    #[test]
    fn limiter_is_transparent_below_ceiling() {
        let mut l = Limiter::new(RATE);
        let x = sine(440.0, 0.25, 2048);
        let mut y = vec![0.0; x.len()];
        l.process(&LimiterParams::default(), &x, &mut y);
        let d = l.latency();
        for i in d..x.len() {
            assert!((y[i] - x[i - d]).abs() < 1e-6);
        }
    }
}
