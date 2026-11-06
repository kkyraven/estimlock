//! UI-independent engine for estimlock.
//!
//! [`machine`] holds every rule and is pure. [`runner`] runs it on a thread against a
//! [`device::VolumeDevice`]. Start here: `wasapi::spawn_runner`, then `Runner::send` and
//! `Runner::snapshot`.

pub mod config;
pub mod curve;
pub mod device;
pub mod machine;
pub mod runner;
#[cfg(windows)]
pub mod wasapi;
pub mod wave;
