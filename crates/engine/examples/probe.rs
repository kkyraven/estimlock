//! Measures real WASAPI behaviour to pick the tick rate and maximum effect Hz.
//!
//! Lists every render endpoint read-only. Writes ONLY to the virtual endpoint named
//! `Steam Streaming Speakers`, refuses to run if it is missing, and restores it afterwards.
//!
//!     cargo run --release -p estimlock-engine --example probe

use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use estimlock_engine::config::{Config, PersistedState, TICK_HZ};
use estimlock_engine::device::{DeviceError, DeviceOpener, VolumeDevice};
use estimlock_engine::machine::Command;
use estimlock_engine::wasapi::{
    Endpoint, WasapiDevice, WasapiOpener, list_endpoints, spawn_runner,
};

const WRITE_TARGET: &str = "Steam Streaming Speakers";
const WRITES: usize = 500;

/// Puts the write target back on drop, so a panic or early return still restores it.
struct Restore {
    device: WasapiDevice,
    volume: f64,
    muted: bool,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let volume = self.device.set_volume(self.volume);
        let muted = self.device.set_muted(self.muted);
        println!("restored: volume {volume:?}, mute {muted:?}");
    }
}

fn read_all(
    opener: &mut WasapiOpener,
    endpoints: &[Endpoint],
) -> Result<Vec<(f64, bool)>, DeviceError> {
    endpoints
        .iter()
        .map(|endpoint| {
            let device = opener.open(&endpoint.id)?;
            Ok((device.volume()?, device.muted()?))
        })
        .collect()
}

fn report(label: &str, samples: &mut [Duration]) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    println!(
        "{label}: n={} median={:?} p99={:?} max={:?}",
        samples.len(),
        at(0.5),
        at(0.99),
        samples[samples.len() - 1],
    );
}

fn probe_writes(device: &mut WasapiDevice) -> Result<(), DeviceError> {
    let mut writes = Vec::with_capacity(WRITES);
    let mut reads = Vec::with_capacity(WRITES);
    let mut stale = 0;
    let mut worst_error: f64 = 0.0;

    for index in 0..WRITES {
        // A distinct low level each time, so no write can be skipped as a no-op.
        let level = 0.10 + 0.20 * (index as f64 / WRITES as f64);
        let started = Instant::now();
        device.set_volume(level)?;
        writes.push(started.elapsed());

        let started = Instant::now();
        let read = device.volume()?;
        reads.push(started.elapsed());

        let error = (read - level).abs();
        worst_error = worst_error.max(error);
        if error > 1e-3 {
            stale += 1;
        }
    }

    report("set_volume", &mut writes);
    report("volume (GetState + read)", &mut reads);
    println!("read right after write: {stale}/{WRITES} stale, worst error {worst_error:.7}");

    // A fine sweep shows whether the endpoint rounds levels.
    let mut worst_step: f64 = 0.0;
    for step in 0..=200 {
        let level = 0.05 + f64::from(step) * 0.001;
        device.set_volume(level)?;
        worst_step = worst_step.max((device.volume()? - level).abs());
    }
    println!("rounding sweep 0.05..0.25 by 0.001: worst error {worst_step:.7}");

    let before = device.muted()?;
    device.set_muted(!before)?;
    let flipped = device.muted()?;
    device.set_muted(before)?;
    println!(
        "mute round trip: {before} -> {flipped} -> {}",
        device.muted()?
    );
    Ok(())
}

/// How closely a sleeping loop holds the tick period, on a real scheduler.
fn probe_tick_timing() {
    let period = Duration::from_secs_f64(1.0 / f64::from(TICK_HZ));
    let mut gaps = Vec::with_capacity(WRITES);
    for _ in 0..WRITES {
        let started = Instant::now();
        sleep(period);
        gaps.push(started.elapsed());
    }
    report(&format!("sleep({period:?})"), &mut gaps);
}

/// Runs the real runner thread against the write target, as the app will.
fn probe_runner(device: &mut WasapiDevice, id: &str) -> Result<(), DeviceError> {
    let settle = Duration::from_millis(200);
    device.set_volume(0.30)?;

    let config = Config {
        device_id: Some(id.into()),
        ..Config::default()
    };
    let runner = spawn_runner(PersistedState {
        config,
        ..Default::default()
    });
    runner.send(Command::SetOn(true));
    sleep(settle);
    let state = runner.snapshot();
    println!(
        "runner on={} base={:.3} device={:?}",
        state.on, state.base, state.device
    );

    device.set_volume(0.10)?;
    sleep(settle);
    println!(
        "external decrease to 0.10: device reads {:.3}",
        device.volume()?
    );

    device.set_muted(true)?;
    sleep(settle);
    println!("external mute: muted reads {}", device.muted()?);

    device.set_volume(0.40)?;
    sleep(settle);
    println!("external raise to 0.40: base {:.3}", runner.snapshot().base);

    device.set_volume(0.90)?;
    sleep(settle);
    let base = runner.snapshot().base;
    println!(
        "external raise to 0.90: base {base:.3}, device reads {:.3}",
        device.volume()?
    );
    Ok(())
}

fn run() -> Result<ExitCode, DeviceError> {
    let endpoints = list_endpoints()?;
    let mut opener = WasapiOpener::default();
    let before = read_all(&mut opener, &endpoints)?;

    println!("render endpoints (read only):");
    for (endpoint, (volume, muted)) in endpoints.iter().zip(&before) {
        let hardware = opener.open(&endpoint.id)?.hardware_volume()?;
        println!(
            "  {:5.1}% muted={muted:5} hardware={hardware:5} {}",
            volume * 100.0,
            endpoint.name,
        );
    }

    let matches: Vec<&Endpoint> = endpoints
        .iter()
        .filter(|endpoint| endpoint.name.contains(WRITE_TARGET))
        .collect();
    let [endpoint] = matches[..] else {
        println!(
            "refusing to write: {} endpoints match {WRITE_TARGET:?}",
            matches.len()
        );
        return Ok(ExitCode::FAILURE);
    };

    println!("write target: {}", endpoint.name);
    let device = opener.open(&endpoint.id)?;
    let (volume, muted) = (device.volume()?, device.muted()?);
    println!("original: volume {volume:.7}, muted {muted}");

    let mut target = Restore {
        device,
        volume,
        muted,
    };
    probe_writes(&mut target.device)?;
    probe_tick_timing();
    probe_runner(&mut target.device, &endpoint.id)?;
    drop(target);

    let after = read_all(&mut opener, &endpoints)?;
    let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
    println!("endpoints changed after restore: {moved}");
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    run().unwrap_or_else(|error| {
        println!("probe failed: {error}");
        ExitCode::FAILURE
    })
}
