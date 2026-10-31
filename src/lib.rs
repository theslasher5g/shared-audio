//! shared-audio: Systemaudio (WASAPI-Loopback) auf mehrere Ausgabegeräte duplizieren.
//!
//! Plattformunabhängiger Kern (Delay, Kanäle, Resampling, Drift, Ring, Config, Parameter)
//! plus der Windows-spezifische Teil (WASAPI, Engine, GUI).

pub mod channels;
pub mod config;
pub mod delay;
pub mod drift;
pub mod params;
pub mod resample;
pub mod ring;

#[cfg(windows)]
pub mod cli;
#[cfg(windows)]
pub mod devices;
#[cfg(windows)]
pub mod engine;
#[cfg(windows)]
pub mod gui;
