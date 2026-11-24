//! `libarctis_dsp.so` — a small LADSPA plugin library with the dynamics
//! processors PipeWire's filter-chain does not ship as builtins.
//!
//! The microphone chain (see `mic.rs`) loads these next to PipeWire's own
//! biquads and RNNoise. Every plugin is mono, `f32`, real-time safe (no
//! allocation in `run`) and exact pass-through at its neutral settings, so a
//! stage can be "bypassed" by a parameter change instead of a graph rebuild.
//!
//! | label              | purpose                                             |
//! | ------------------ | --------------------------------------------------- |
//! | `arctis_gate`      | downward expander / noise gate with hold            |
//! | `arctis_comp`      | soft-knee feed-forward compressor with make-up gain |
//! | `arctis_deesser`   | split-band sibilance reducer                        |
//! | `arctis_limiter`   | 1.5 ms look-ahead brick-wall limiter + output gain  |

pub mod dsp;
mod ladspa;

pub use ladspa::ladspa_descriptor;
