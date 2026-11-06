//! The state machine: every rule of the spec, with no OS calls.
//!
//! Time and randomness are passed in. The runner feeds it device readings through
//! [`Machine::tick`] and carries out the [`DeviceWrite`] it returns.

use std::collections::VecDeque;

use serde::Serialize;

use crate::config::{Config, EventConfig, PersistedState, clamped};
use crate::curve::recovery_gain;
use crate::wave::Effect;

/// Unlock countdown, fixed by the spec and deliberately not configurable.
pub const UNLOCK_SECS: f64 = 30.0;

/// Levels closer than this are equal. Far below the 0.01 of a slider step, far above f32 noise.
const LEVEL_EPSILON: f64 = 1e-4;

/// Longest gap one tick may ramp across. A PC that slept for hours must not wake at target.
const MAX_RAMP_STEP_SECS: f64 = 0.25;

/// A longer gap between ticks means the PC slept or the engine stalled. The device may have
/// reset itself meanwhile, so the next reading counts as a device coming back.
const MAX_TICK_GAP_SECS: f64 = 1.0;

/// How long a reading may still be the device catching up with the app's own writes, on
/// endpoints that apply or report a write late. Slower ones can still ratchet.
const OWN_LEVEL_SECS: f64 = 0.25;

/// How far a level is stepped down per tick on a device that rounds it back up. One slider step.
const STEP_DOWN: f64 = 0.01;

/// Uniform randomness in 0.0..1.0, injected so tests are deterministic.
pub trait Random {
    fn unit(&mut self) -> f64;
}

impl Random for fastrand::Rng {
    fn unit(&mut self) -> f64 {
        self.f64()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Begging,
    Came,
}

/// Which effect is active. An event stage beats ramp and ramp beats hold, because an event is
/// the short deliberate thing happening now, while a ramp can run underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Hold,
    Ramp,
    Spike,
    Recovery,
}

#[derive(Debug, Clone)]
pub enum Command {
    SetOn(bool),
    Lock,
    BeginUnlock,
    CancelUnlock,
    ToggleRamp,
    Press(EventKind),
    /// Sets the level and the held base, capped at the ceiling. Refused while locked.
    SetLevel(f64),
    UpdateConfig(Box<Config>),
    /// Silences the device and turns Off, ignoring the lock. Needs `Config::panic_hotkey`.
    Panic,
}

/// Why a command did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Off and config changes are refused while locked.
    Locked,
    /// Unlock was pressed while not locked.
    NotLocked,
    /// The ramp and event buttons only work while On.
    Off,
    /// The target device can only change while Off.
    DeviceChangeWhileOn,
    /// A press came during an event it cannot pre-empt.
    EventActive,
    CoolingDown,
    /// Panic was sent with `Config::panic_hotkey` off.
    PanicDisabled,
}

/// One tick's view of the device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceReading {
    pub volume: f64,
    pub muted: bool,
}

/// What the device must do. Set the volume before unmuting, so a stale level is never heard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceWrite {
    pub volume: f64,
    pub unmute: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DeviceStatus {
    /// No endpoint id configured.
    NotSelected,
    /// Waiting for the first open attempt.
    Connecting,
    Connected,
    /// The device failed or is missing. The runner keeps retrying.
    Lost {
        error: String,
    },
}

/// Seconds until each event can be pressed again. 0 when ready.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cooldowns {
    pub begging: f64,
    pub came: f64,
}

/// Everything the shell shows and saves.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// What the device reports: the latest reading or readback, the commanded level only
    /// until the device has answered.
    pub output: f64,
    /// The live held level. 0 until set or adopted.
    pub base: f64,
    pub stage: Stage,
    pub event: Option<EventKind>,
    /// A punished press is waiting out its delay.
    pub pending: bool,
    pub ramp_running: bool,
    pub on: bool,
    pub locked: bool,
    /// Seconds left on a running unlock countdown.
    pub unlock_remaining: Option<f64>,
    pub cooldowns: Cooldowns,
    pub device: DeviceStatus,
    /// Save this whenever it changes.
    pub persisted: PersistedState,
}

/// The spike half of a punished press.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SpikePlan {
    gain: f64,
    secs: f64,
    delay: f64,
}

/// What one press will do.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PressPlan {
    spike: Option<SpikePlan>,
    drop: f64,
}

/// Rolls a press. This is the one place that decides what a press does.
fn resolve_press(config: &EventConfig, rng: &mut impl Random) -> PressPlan {
    if rng.unit() >= config.chance {
        // Spared: skips the spike but still drops. Use `drop: 0.0` to leave the volume alone.
        return PressPlan {
            spike: None,
            drop: config.drop,
        };
    }
    let mut between = |min: f64, max: f64| min + (max - min) * rng.unit();
    let spike = SpikePlan {
        gain: 1.0 + between(config.spike_min, config.spike_max),
        secs: between(config.spike_min_secs, config.spike_max_secs),
        delay: between(config.delay_min_secs, config.delay_max_secs),
    };
    PressPlan {
        spike: Some(spike),
        drop: config.drop,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Delay { spike: SpikePlan, until: f64 },
    Spike { gain: f64, until: f64 },
    Recovery { since: f64 },
}

/// A running event. Holds its own numbers, so a config change cannot warp it midway.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ActiveEvent {
    kind: EventKind,
    phase: Phase,
    drop: f64,
    recover_secs: f64,
    curve_k: f64,
}

impl ActiveEvent {
    fn gain(&self, now: f64) -> f64 {
        match self.phase {
            Phase::Delay { .. } => 1.0,
            Phase::Spike { gain, .. } => gain,
            Phase::Recovery { since } => {
                let p = if self.recover_secs > 0.0 {
                    (now - since) / self.recover_secs
                } else {
                    1.0
                };
                recovery_gain(p, self.drop, self.curve_k)
            }
        }
    }
}

/// The last write: the level asked for, and what the device reported back. They differ on
/// hardware with coarse steps, and comparing against only one would read that as a raise.
#[derive(Debug, Clone, Copy)]
struct Written {
    commanded: f64,
    readback: f64,
}

impl Written {
    fn differs_from(&self, volume: f64) -> bool {
        !same_level(volume, self.commanded) && !same_level(volume, self.readback)
    }

    fn peak(&self) -> f64 {
        self.commanded.max(self.readback)
    }
}

fn same_level(a: f64, b: f64) -> bool {
    (a - b).abs() <= LEVEL_EPSILON
}

/// A write owed after Off, see `Machine::stop`. `aim` starts at `level` and steps down if the
/// device rounds it back up, so it always ends, and Off never keeps fighting the slider.
#[derive(Debug, Clone, Copy)]
struct Settle {
    level: f64,
    aim: f64,
}

#[derive(Debug, Clone)]
pub struct Machine {
    config: Config,
    /// The set level, 0 if unset. Can sit above a lowered ceiling: see `capped_level`.
    level: f64,
    base: f64,
    output: f64,
    on: bool,
    locked: bool,
    unlock_at: Option<f64>,
    ramp_running: bool,
    event: Option<ActiveEvent>,
    begging_ready_at: f64,
    came_ready_at: f64,
    stage: Stage,
    stage_since: f64,
    last_tick: f64,
    /// `None` until the first device read after turning On.
    written: Option<Written>,
    device_lost: bool,
    /// Peaks of the writes replaced in the last `OWN_LEVEL_SECS`, as (time replaced, level).
    /// Timed from the replacing, as a level counts for as long as it was held.
    own_levels: VecDeque<(f64, f64)>,
    last_reading: f64,
    /// How far below the ceiling writes are held, because the device rounds up past it.
    trim: f64,
    settle: Option<Settle>,
}

impl Machine {
    /// Starts Off, unless a locked session was On: that resumes, so a restart is no way out.
    /// A locked restart keeps the live base. An unlocked one starts from the set level.
    pub fn new(state: PersistedState) -> Self {
        let config = state.config.sanitized();
        let level = clamped(state.level, 0.0, 1.0);
        let start = if state.locked { state.base } else { level };
        let base = clamped(start, 0.0, config.ceiling);
        let on = state.locked && state.on;
        Self {
            config,
            level,
            base,
            output: base,
            on,
            locked: state.locked,
            unlock_at: None,
            ramp_running: on && state.ramp_running,
            event: None,
            begging_ready_at: 0.0,
            came_ready_at: 0.0,
            stage: Stage::Hold,
            stage_since: 0.0,
            last_tick: 0.0,
            written: None,
            device_lost: false,
            own_levels: VecDeque::new(),
            last_reading: 0.0,
            trim: 0.0,
            settle: None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Runs one command. All locking rules are enforced here, not in the shell.
    pub fn apply(
        &mut self,
        command: Command,
        now: f64,
        rng: &mut impl Random,
    ) -> Result<(), Rejection> {
        match command {
            Command::SetOn(true) => self.start(now),
            Command::SetOn(false) if self.locked => return Err(Rejection::Locked),
            Command::SetOn(false) => self.stop(now),
            Command::Lock => self.locked = true,
            Command::BeginUnlock if !self.locked => return Err(Rejection::NotLocked),
            Command::BeginUnlock => {
                self.unlock_at.get_or_insert(now + UNLOCK_SECS);
            }
            Command::CancelUnlock => self.unlock_at = None,
            Command::ToggleRamp if !self.on => return Err(Rejection::Off),
            Command::ToggleRamp => self.ramp_running = !self.ramp_running,
            Command::Press(kind) => return self.press(kind, now, rng),
            Command::SetLevel(_) if self.locked => return Err(Rejection::Locked),
            Command::SetLevel(level) => {
                self.level = clamped(level, 0.0, self.config.ceiling);
                self.base = self.level;
            }
            Command::UpdateConfig(config) => return self.update_config(*config),
            Command::Panic if !self.config.panic_hotkey => return Err(Rejection::PanicDisabled),
            Command::Panic => self.panic(now),
        }
        Ok(())
    }

    /// Advances to `now`. `reading` is `None` while the device is unavailable. The runner
    /// must perform the returned write, then report the result through [`Self::note_readback`].
    pub fn tick(&mut self, now: f64, reading: Option<DeviceReading>) -> Option<DeviceWrite> {
        let gap = now - self.last_tick;
        let elapsed = gap.clamp(0.0, MAX_RAMP_STEP_SECS);
        self.last_tick = now;
        self.own_levels.retain(|(at, _)| now - at <= OWN_LEVEL_SECS);
        self.advance_unlock(now);
        self.advance_event(now);
        self.update_stage(now);

        let Some(reading) = reading else {
            self.device_lost = self.on;
            return None;
        };
        if !self.on {
            return self.tick_off(reading);
        }
        let returned = std::mem::take(&mut self.device_lost) || gap > MAX_TICK_GAP_SECS;
        let external = self.written.is_none_or(|w| w.differs_from(reading.volume));

        if let Some(raised) = self.raised_base(reading.volume, returned, now) {
            self.base = raised.min(self.config.ceiling);
        }
        self.step_down_if_over(reading.volume);
        self.last_reading = reading.volume;
        self.advance_ramp(elapsed);

        let output = self.compute_output(now);
        self.output = reading.volume;
        let moved = self
            .written
            .is_none_or(|w| !same_level(output, w.commanded));
        if !(external || returned || moved || reading.muted) {
            return None;
        }
        if !reading.muted && same_level(output, reading.volume) {
            self.record_write(output, reading.volume, now);
            return None;
        }
        self.output = output;
        self.record_write(output, output, now);
        Some(DeviceWrite {
            volume: output,
            unmute: reading.muted,
        })
    }

    /// Records what the device reports right after a write.
    pub fn note_readback(&mut self, volume: f64) {
        self.output = volume;
        if let Some(written) = &mut self.written {
            written.readback = volume;
        }
    }

    fn record_write(&mut self, commanded: f64, readback: f64, now: f64) {
        let written = Written {
            commanded,
            readback,
        };
        if let Some(replaced) = self.written.replace(written) {
            self.own_levels.push_back((now, replaced.peak()));
        }
    }

    /// Turns Off for app exit, ignoring the lock. Crate-private so the shell cannot use it to
    /// get around the lock: refusing to quit while locked is the shell's job, via `locked`.
    pub(crate) fn shutdown(&mut self, now: f64) {
        self.stop(now);
    }

    pub fn persisted(&self) -> PersistedState {
        PersistedState {
            config: self.config.clone(),
            locked: self.locked,
            level: self.level,
            base: self.base,
            on: self.on,
            ramp_running: self.ramp_running,
        }
    }

    pub fn snapshot(&self, now: f64, device: DeviceStatus) -> Snapshot {
        let remaining = |at: f64| (at - now).max(0.0);
        Snapshot {
            output: self.output,
            base: self.base,
            stage: self.stage,
            event: self.event.map(|event| event.kind),
            pending: self
                .event
                .is_some_and(|event| matches!(event.phase, Phase::Delay { .. })),
            ramp_running: self.ramp_running,
            on: self.on,
            locked: self.locked,
            // Capped because float error can put a fresh countdown a hair over.
            unlock_remaining: self.unlock_at.map(|at| remaining(at).min(UNLOCK_SECS)),
            cooldowns: Cooldowns {
                begging: remaining(self.begging_ready_at),
                came: remaining(self.came_ready_at),
            },
            device,
            persisted: self.persisted(),
        }
    }

    fn start(&mut self, now: f64) {
        if !self.on {
            self.on = true;
            self.settle = None;
            self.trim = 0.0;
            self.stage_since = now;
        }
    }

    /// Turns Off and takes the device to silence, even if it was already Off. The lock stays,
    /// but a session stopped this way does not resume on restart, as `on` is now false.
    fn panic(&mut self, now: f64) {
        self.stop(now);
        self.settle = Some(Settle {
            level: 0.0,
            aim: 0.0,
        });
    }

    /// Stops enforcing. Off leaves the device alone, with one exception: a device above base
    /// is brought down to it, so a spike cannot stay stuck on. It never raises the level.
    /// Unlocked, base returns to the set level, so a ramp's peak is not the next start.
    fn stop(&mut self, now: f64) {
        if !self.on {
            return;
        }
        if self.written.is_some_and(|w| w.commanded > self.base) {
            self.settle = Some(Settle {
                level: self.base,
                aim: self.base,
            });
        }
        self.on = false;
        self.ramp_running = false;
        self.written = None;
        self.device_lost = false;
        self.own_levels.clear();
        self.end_event(now);
        if !self.locked {
            self.base = self.capped_level();
        }
    }

    /// Writes until a reading confirms the device came down. A failed write therefore retries,
    /// and a device that comes back still at spike level is settled when it does.
    fn tick_off(&mut self, reading: DeviceReading) -> Option<DeviceWrite> {
        let falling = reading.volume < self.last_reading - LEVEL_EPSILON;
        self.last_reading = reading.volume;
        self.output = reading.volume;
        let settle = self.settle.as_mut()?;
        if reading.volume <= settle.level + LEVEL_EPSILON {
            self.settle = None;
            return None;
        }
        let volume = settle.aim;
        if !falling {
            settle.aim = (volume - STEP_DOWN).max(0.0);
        }
        self.output = volume;
        Some(DeviceWrite {
            volume,
            unmute: false,
        })
    }

    /// The level clamped for use. Stored unclamped, so dragging the ceiling down and back
    /// loses nothing.
    fn capped_level(&self) -> f64 {
        self.level.min(self.config.ceiling)
    }

    /// The base a reading raises to, if it is a raise. A raise has to beat everything the app
    /// itself asked for or read back lately, or a device catching up with its own writes would
    /// ratchet. So a lowering that stays above base, such as pulling the slider down during a
    /// spike, is not one: it gets reasserted. Neither is a device coming back: it may have reset
    /// itself to any level. Nor is the first reading after On, which is pulled to base, unless
    /// base is unset and adopts it.
    ///
    /// During a spike the reading includes the spike's gain, which must not stay in base.
    fn raised_base(&self, volume: f64, returned: bool, now: f64) -> Option<f64> {
        let Some(written) = self.written else {
            return (self.base <= 0.0).then_some(volume);
        };
        let own_peak = self
            .own_levels
            .iter()
            .fold(written.peak(), |peak, level| peak.max(level.1));
        if returned || volume <= self.base || volume <= own_peak + LEVEL_EPSILON {
            return None;
        }
        let gain = self.event.map_or(1.0, |event| event.gain(now).max(1.0));
        Some((volume / gain).max(self.base))
    }

    /// Holds writes lower while the device reads above the ceiling because of a write of ours:
    /// a coarse device can round one up past it, at once or a moment later. A reading that
    /// matches neither the readback nor the last reading is an external raise, which the next
    /// write pulls back. Trimming for those would let the slider silence a locked session.
    fn step_down_if_over(&mut self, volume: f64) {
        let ceiling = self.config.ceiling;
        let rounded_up = self.written.is_some_and(|w| {
            w.commanded <= ceiling + LEVEL_EPSILON
                && (same_level(volume, w.readback) || same_level(volume, self.last_reading))
        });
        if rounded_up && volume > ceiling + LEVEL_EPSILON {
            self.trim += STEP_DOWN;
        }
    }

    fn compute_output(&self, now: f64) -> f64 {
        let event_gain = self.event.map_or(1.0, |event| event.gain(now));
        let effect = self.effect(self.stage).gain(now - self.stage_since);
        let cap = (self.config.ceiling - self.trim).max(0.0);
        clamped(self.base * event_gain * effect, 0.0, cap)
    }

    fn effect(&self, stage: Stage) -> Effect {
        let effects = &self.config.effects;
        match stage {
            Stage::Hold => effects.hold,
            Stage::Ramp => effects.ramp,
            Stage::Spike => effects.spike,
            Stage::Recovery => effects.recovery,
        }
    }

    fn advance_unlock(&mut self, now: f64) {
        if self.unlock_at.is_some_and(|at| now >= at) {
            self.unlock_at = None;
            self.locked = false;
            if !self.on {
                self.base = self.capped_level();
            }
        }
    }

    fn advance_ramp(&mut self, elapsed: f64) {
        if !self.ramp_running {
            return;
        }
        let target = self.config.ramp.target.min(self.config.ceiling);
        if self.base < target {
            self.base = (self.base + self.config.ramp.rate / 60.0 * elapsed).min(target);
        }
        self.ramp_running = self.base < target;
    }

    fn advance_event(&mut self, now: f64) {
        let Some(event) = &mut self.event else { return };
        if let Phase::Delay { spike, until } = event.phase
            && now >= until
        {
            event.phase = Phase::Spike {
                gain: spike.gain,
                until: until + spike.secs,
            };
        }
        if let Phase::Spike { until, .. } = event.phase
            && now >= until
        {
            // From this tick, not the spike's exact end, so a full drop really reaches silence.
            event.phase = Phase::Recovery { since: now };
        }
        if let Phase::Recovery { since } = event.phase
            && now >= since + event.recover_secs
        {
            self.end_event(now);
        }
    }

    /// Ends any event and starts its cooldown.
    fn end_event(&mut self, now: f64) {
        if let Some(event) = self.event.take() {
            let secs = self.event_config(event.kind).cooldown_secs;
            match event.kind {
                EventKind::Begging => self.begging_ready_at = now + secs,
                EventKind::Came => self.came_ready_at = now + secs,
            }
        }
    }

    fn update_stage(&mut self, now: f64) {
        let stage = match self.event.map(|event| event.phase) {
            Some(Phase::Spike { .. }) => Stage::Spike,
            Some(Phase::Recovery { .. }) => Stage::Recovery,
            // A delay shows nothing yet, so the effect underneath keeps running unbroken.
            Some(Phase::Delay { .. }) | None if self.ramp_running => Stage::Ramp,
            Some(Phase::Delay { .. }) | None => Stage::Hold,
        };
        if stage != self.stage {
            self.stage = stage;
            self.stage_since = now;
        }
    }

    fn event_config(&self, kind: EventKind) -> &EventConfig {
        match kind {
            EventKind::Begging => &self.config.begging,
            EventKind::Came => &self.config.came,
        }
    }

    fn press(&mut self, kind: EventKind, now: f64, rng: &mut impl Random) -> Result<(), Rejection> {
        if !self.on {
            return Err(Rejection::Off);
        }
        let pre_empts = kind == EventKind::Came
            && self
                .event
                .is_some_and(|event| event.kind == EventKind::Begging);
        if self.event.is_some() && !pre_empts {
            return Err(Rejection::EventActive);
        }
        let ready_at = match kind {
            EventKind::Begging => self.begging_ready_at,
            EventKind::Came => self.came_ready_at,
        };
        if now < ready_at {
            return Err(Rejection::CoolingDown);
        }

        self.end_event(now);
        let config = self.event_config(kind);
        let plan = resolve_press(config, rng);
        let phase = match plan.spike {
            Some(spike) if spike.delay > 0.0 => Phase::Delay {
                spike,
                until: now + spike.delay,
            },
            Some(spike) => Phase::Spike {
                gain: spike.gain,
                until: now + spike.secs,
            },
            None => Phase::Recovery { since: now },
        };
        self.event = Some(ActiveEvent {
            kind,
            phase,
            drop: plan.drop,
            recover_secs: config.recover_secs,
            curve_k: config.curve_k,
        });
        Ok(())
    }

    fn update_config(&mut self, config: Config) -> Result<(), Rejection> {
        if self.locked {
            return Err(Rejection::Locked);
        }
        if self.on && config.device_id != self.config.device_id {
            return Err(Rejection::DeviceChangeWhileOn);
        }
        let config = config.sanitized();
        if config.device_id != self.config.device_id {
            self.settle = None;
        }
        if config.ceiling != self.config.ceiling {
            self.trim = 0.0;
        }
        self.config = config;
        self.base = if self.on {
            self.base.min(self.config.ceiling)
        } else {
            self.capped_level()
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wave::Wave;

    /// Always rolls the same value: 0.0 punishes at the bottom of each range, 0.999 spares.
    struct Fixed(f64);

    impl Random for Fixed {
        fn unit(&mut self) -> f64 {
            self.0
        }
    }

    const PUNISH: f64 = 0.0;
    const SPARE: f64 = 0.999;

    fn reading(volume: f64) -> Option<DeviceReading> {
        Some(DeviceReading {
            volume,
            muted: false,
        })
    }

    fn test_config() -> Config {
        let mut config = Config {
            ceiling: 1.0,
            ..Config::default()
        };
        config.ramp = crate::config::RampConfig {
            rate: 0.6,
            target: 0.8,
        };
        config.begging = EventConfig {
            chance: 0.5,
            spike_min: 0.25,
            spike_max: 0.25,
            spike_min_secs: 2.0,
            spike_max_secs: 2.0,
            delay_min_secs: 0.0,
            delay_max_secs: 0.0,
            drop: 0.5,
            recover_secs: 10.0,
            curve_k: 20.0,
            cooldown_secs: 0.0,
        };
        config.came = config.begging.clone();
        config
    }

    fn send(machine: &mut Machine, command: Command, now: f64) -> Result<(), Rejection> {
        machine.apply(command, now, &mut Fixed(SPARE))
    }

    /// A machine that is On and holding `level`, at time 0.
    fn holding_with(config: Config, level: f64) -> Machine {
        let mut machine = Machine::new(PersistedState {
            config,
            ..Default::default()
        });
        send(&mut machine, Command::SetOn(true), 0.0).unwrap();
        assert_eq!(machine.tick(0.0, reading(level)), None);
        machine
    }

    fn holding(level: f64) -> Machine {
        holding_with(test_config(), level)
    }

    fn press(machine: &mut Machine, kind: EventKind, roll: f64, now: f64) -> Result<(), Rejection> {
        machine.apply(Command::Press(kind), now, &mut Fixed(roll))
    }

    /// Plays the device: ticks with the last written level and returns the output.
    fn play(machine: &mut Machine, now: f64, device: &mut f64) -> f64 {
        if let Some(write) = machine.tick(now, reading(*device)) {
            *device = write.volume;
        }
        *device
    }

    fn output(machine: &Machine) -> f64 {
        machine.snapshot(0.0, DeviceStatus::Connected).output
    }

    /// A device with its own timing, for `drive`.
    trait Endpoint {
        fn value(&self) -> f64;
        /// Called once per tick, before the reading.
        fn advance(&mut self);
        fn write(&mut self, volume: f64);
    }

    /// Ticks from `from` to `to` at 50 Hz, as the runner does, with no external change.
    fn drive(machine: &mut Machine, device: &mut impl Endpoint, from: f64, to: f64) {
        let ticks = ((to - from) * 50.0).round() as u32;
        for tick in 0..ticks {
            device.advance();
            let now = from + f64::from(tick) * 0.02;
            if let Some(write) = machine.tick(now, reading(device.value())) {
                device.write(write.volume);
                machine.note_readback(device.value());
            }
        }
    }

    /// Applies each write `lag` ticks late, with reads showing the old level until then.
    struct Delayed {
        value: f64,
        lag: u32,
        pending: VecDeque<(u32, f64)>,
    }

    impl Delayed {
        fn new(value: f64, lag: u32) -> Self {
            Self {
                value,
                lag,
                pending: VecDeque::new(),
            }
        }
    }

    impl Endpoint for Delayed {
        fn value(&self) -> f64 {
            self.value
        }

        fn advance(&mut self) {
            for (left, _) in &mut self.pending {
                *left -= 1;
            }
            while let Some((_, value)) = self.pending.front().filter(|(left, _)| *left == 0) {
                self.value = *value;
                self.pending.pop_front();
            }
        }

        fn write(&mut self, volume: f64) {
            self.pending.push_back((self.lag, volume));
        }
    }

    /// Moves toward the last write by 0.01 a tick.
    struct Slewing {
        value: f64,
        target: f64,
    }

    impl Slewing {
        fn new(value: f64) -> Self {
            Self {
                value,
                target: value,
            }
        }
    }

    impl Endpoint for Slewing {
        fn value(&self) -> f64 {
            self.value
        }

        fn advance(&mut self) {
            self.value += (self.target - self.value).clamp(-0.01, 0.01);
        }

        fn write(&mut self, volume: f64) {
            self.target = volume;
        }
    }

    /// Rounds to the nearest step at once, like hardware with coarse volume steps.
    struct Rounding {
        value: f64,
        step: f64,
    }

    impl Rounding {
        fn new(value: f64, step: f64) -> Self {
            Self { value, step }
        }
    }

    impl Endpoint for Rounding {
        fn value(&self) -> f64 {
            self.value
        }

        fn advance(&mut self) {}

        fn write(&mut self, volume: f64) {
            self.value = (volume / self.step).round() * self.step;
        }
    }

    /// Reports a write exactly, then rounds it up to a step on the next tick.
    struct DeferredRounding {
        value: f64,
        step: f64,
        pending: bool,
    }

    impl DeferredRounding {
        fn new(value: f64, step: f64) -> Self {
            Self {
                value,
                step,
                pending: false,
            }
        }
    }

    impl Endpoint for DeferredRounding {
        fn value(&self) -> f64 {
            self.value
        }

        fn advance(&mut self) {
            if std::mem::take(&mut self.pending) {
                self.value = (self.value / self.step).ceil() * self.step;
            }
        }

        fn write(&mut self, volume: f64) {
            self.value = volume;
            self.pending = true;
        }
    }

    #[test]
    fn on_adopts_the_device_level_without_writing() {
        let machine = holding(0.37);
        assert_eq!(machine.base, 0.37);
        assert_eq!(machine.level, 0.0, "adopting does not set the level");
    }

    #[test]
    fn output_clamps_to_ceiling() {
        let mut config = test_config();
        config.ceiling = 0.45;
        let mut machine = holding_with(config, 0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        let write = machine.tick(1.1, reading(0.4)).unwrap();
        assert_eq!(write.volume, 0.45, "0.4 * 1.25 is capped");
    }

    #[test]
    fn external_raise_above_ceiling_is_pulled_back() {
        let mut config = test_config();
        config.ceiling = 0.5;
        let mut machine = holding_with(config, 0.4);
        let write = machine.tick(1.0, reading(0.9)).unwrap();
        assert_eq!(write.volume, 0.5);
        assert_eq!(machine.base, 0.5);
    }

    #[test]
    fn ratchet_raises_base_on_external_raise() {
        let mut machine = holding(0.4);
        assert_eq!(
            machine.tick(1.0, reading(0.6)),
            None,
            "already at the new level"
        );
        assert_eq!(machine.base, 0.6);
    }

    #[test]
    fn external_decrease_is_reasserted() {
        let mut machine = holding(0.4);
        let write = machine.tick(1.0, reading(0.2)).unwrap();
        assert_eq!(
            write,
            DeviceWrite {
                volume: 0.4,
                unmute: false
            }
        );
        assert_eq!(machine.base, 0.4);
    }

    #[test]
    fn mute_is_reasserted() {
        let mut machine = holding(0.4);
        let muted = Some(DeviceReading {
            volume: 0.4,
            muted: true,
        });
        let write = machine.tick(1.0, muted).unwrap();
        assert_eq!(
            write,
            DeviceWrite {
                volume: 0.4,
                unmute: true
            }
        );
    }

    #[test]
    fn raise_during_recovery_below_base_is_reasserted() {
        let mut machine = holding(0.5);
        press(&mut machine, EventKind::Begging, SPARE, 1.0).unwrap();
        let mut device = 0.5;
        let output = play(&mut machine, 1.0, &mut device);
        assert!(output < 0.3, "recovery starts low, got {output}");

        let write = machine.tick(1.02, reading(output + 0.1)).unwrap();
        assert!(
            write.volume < output + 0.05,
            "nudge was undone, got {}",
            write.volume
        );
        assert_eq!(machine.base, 0.5);
    }

    #[test]
    fn lowering_during_a_spike_does_not_ratchet() {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        machine.tick(1.0, reading(0.4)).unwrap();
        let write = machine.tick(1.1, reading(0.45)).unwrap();
        assert_eq!(write.volume, 0.5, "spike is reasserted");
        assert_eq!(machine.base, 0.4);
    }

    #[test]
    fn own_write_is_not_mistaken_for_a_raise_when_device_rounds_up() {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        machine.tick(1.0, reading(0.4)).unwrap();
        machine.note_readback(0.52);
        assert_eq!(
            output(&machine),
            0.52,
            "shows the device, not the 0.5 asked for"
        );
        assert_eq!(machine.tick(1.1, reading(0.52)), None);
        assert_eq!(machine.base, 0.4);
        assert_eq!(output(&machine), 0.52);
    }

    #[test]
    fn a_write_rounded_past_the_ceiling_steps_down_until_it_reads_under() {
        let mut config = test_config();
        config.ceiling = 0.55;
        let mut machine = Machine::new(PersistedState {
            config,
            level: 0.55,
            ..Default::default()
        });
        send(&mut machine, Command::SetOn(true), 0.0).unwrap();
        let mut device = Rounding::new(0.3, 0.15);
        drive(&mut machine, &mut device, 0.0, 0.2);

        let settled = device.value();
        assert!(
            (settled - 0.45).abs() < 1e-9,
            "highest step under, got {settled}"
        );
        assert_eq!(output(&machine), settled);
        assert_eq!(machine.base, 0.55, "the device is capped, not the session");
        assert_eq!(
            machine.tick(0.2, reading(settled)),
            None,
            "and it stays settled"
        );
    }

    #[test]
    fn a_write_rounded_past_the_ceiling_later_also_steps_down() {
        let mut config = test_config();
        config.ceiling = 0.55;
        let mut machine = Machine::new(PersistedState {
            config,
            level: 0.55,
            ..Default::default()
        });
        send(&mut machine, Command::SetOn(true), 0.0).unwrap();
        let mut device = DeferredRounding::new(0.3, 0.15);
        drive(&mut machine, &mut device, 0.0, 1.0);
        assert!(device.value() <= 0.55, "device at {}", device.value());
    }

    #[test]
    fn dragging_the_slider_past_the_ceiling_does_not_lower_the_level_held() {
        let mut config = test_config();
        config.ceiling = 0.5;
        let mut machine = holding_with(config, 0.5);
        for tick in 1..200 {
            let dragged = 0.6 + f64::from(tick) * 0.001;
            let write = machine
                .tick(f64::from(tick) * 0.02, reading(dragged))
                .unwrap();
            assert_eq!(write.volume, 0.5);
            machine.note_readback(write.volume);
        }
    }

    #[test]
    fn a_lower_ceiling_is_not_mistaken_for_rounding() {
        let mut machine = holding(0.6);
        let config = Config {
            ceiling: 0.5,
            ..test_config()
        };
        send(&mut machine, Command::UpdateConfig(Box::new(config)), 0.0).unwrap();
        assert_eq!(machine.tick(0.02, reading(0.6)).unwrap().volume, 0.5);
    }

    fn spike_for_ten_seconds() -> Config {
        let mut config = test_config();
        config.begging.spike_min_secs = 10.0;
        config.begging.spike_max_secs = 10.0;
        config.begging.recover_secs = 5.0;
        config
    }

    /// One punished press on `device`, from the spike through the recovery, nothing external.
    fn ride_out_a_spike(config: Config, device: &mut impl Endpoint) -> f64 {
        let mut machine = holding_with(config, device.value());
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        drive(&mut machine, device, 1.0, 30.0);
        assert_eq!(machine.event, None, "event finished");
        machine.base
    }

    #[test]
    fn delayed_writes_do_not_ratchet() {
        let sine = Effect {
            wave: Wave::Sine,
            hz: 2.0,
            depth: 0.3,
        };
        for lag in [1, 2, 4, 12] {
            for effect in [Effect::default(), sine] {
                let mut config = spike_for_ten_seconds();
                config.effects.spike = effect;
                let base = ride_out_a_spike(config, &mut Delayed::new(0.4, lag));
                assert!(
                    (base - 0.4).abs() < 1e-6,
                    "lag {lag} {effect:?}: base {base}"
                );
            }
        }
    }

    #[test]
    fn slewing_writes_do_not_ratchet() {
        let base = ride_out_a_spike(spike_for_ten_seconds(), &mut Slewing::new(0.4));
        assert!((base - 0.4).abs() < 1e-6, "base {base}");
    }

    /// The device reports the write exactly, then rounds it up a tick later. That cannot be told
    /// from a small real raise, but the spike's gain is divided out, so one press costs at most
    /// one device step instead of running to the ceiling.
    #[test]
    fn deferred_rounding_cannot_climb_more_than_a_step() {
        let mut device = DeferredRounding::new(0.4, 0.04);
        let base = ride_out_a_spike(spike_for_ten_seconds(), &mut device);
        assert!(base <= 0.44 + 1e-6, "base {base}");
    }

    #[test]
    fn a_ramp_under_a_spike_stops_at_its_target_on_a_delayed_endpoint() {
        let mut config = spike_for_ten_seconds();
        config.ramp = crate::config::RampConfig {
            rate: 0.3,
            target: 0.42,
        };
        let mut device = Delayed::new(0.4, 2);
        let mut machine = holding_with(config, 0.4);
        send(&mut machine, Command::ToggleRamp, 0.0).unwrap();
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        drive(&mut machine, &mut device, 1.0, 30.0);
        assert!((machine.base - 0.42).abs() < 1e-6, "base {}", machine.base);
    }

    #[test]
    fn a_genuine_raise_during_a_spike_ratchets_without_the_spike_in_it() {
        let mut machine = holding_with(spike_for_ten_seconds(), 0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        machine.tick(1.0, reading(0.4)).unwrap();
        machine.note_readback(0.5);

        assert_eq!(
            machine.tick(1.5, reading(0.75)),
            None,
            "already where it should be"
        );
        assert!(
            (machine.base - 0.6).abs() < 1e-9,
            "0.75 / 1.25, got {}",
            machine.base
        );
    }

    #[test]
    fn a_genuine_raise_during_recovery_ratchets() {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, SPARE, 1.0).unwrap();
        let write = machine.tick(1.0, reading(0.4)).unwrap();
        machine.note_readback(write.volume);
        machine.tick(1.5, reading(0.8));
        assert_eq!(machine.base, 0.8);
    }

    #[test]
    fn a_genuine_raise_ratchets_on_a_delayed_endpoint() {
        let mut device = Delayed::new(0.4, 4);
        let mut machine = holding(0.4);
        drive(&mut machine, &mut device, 0.0, 1.0);
        device.value = 0.6;
        drive(&mut machine, &mut device, 1.0, 2.0);
        assert_eq!(machine.base, 0.6);
    }

    /// The spike was written once, 2s before it ended. It still counts from when it ended.
    #[test]
    fn a_held_level_stays_own_for_the_window_after_it_ends() {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        machine.tick(1.0, reading(0.4)).unwrap();
        machine.tick(2.0, reading(0.5));
        machine.tick(2.99, reading(0.5));

        let write = machine.tick(3.0, reading(0.5)).unwrap();
        assert!(write.volume < 0.4, "recovery began");
        machine.note_readback(write.volume);
        let write = machine.tick(3.02, reading(0.5)).unwrap();
        machine.note_readback(write.volume);
        assert_eq!(machine.base, 0.4, "a late reading of the spike");

        machine.tick(3.3, reading(0.5));
        assert_eq!(
            machine.base, 0.5,
            "the window is over, so now it is a raise"
        );
    }

    #[test]
    fn device_returning_at_a_reset_level_does_not_ratchet() {
        let mut machine = holding(0.4);
        assert_eq!(machine.tick(1.0, None), None);
        let write = machine.tick(2.0, reading(1.0)).unwrap();
        assert_eq!(write.volume, 0.4);
        assert_eq!(machine.base, 0.4);
    }

    #[test]
    fn a_device_reset_during_sleep_does_not_ratchet() {
        let mut machine = holding(0.4);
        let write = machine.tick(8.0 * 3600.0, reading(1.0)).unwrap();
        assert_eq!(write.volume, 0.4);
        assert_eq!(machine.base, 0.4);
    }

    #[test]
    fn a_raise_right_after_a_one_second_gap_still_counts() {
        let mut machine = holding(0.4);
        machine.tick(1.0, reading(0.6));
        assert_eq!(machine.base, 0.6);
    }

    #[test]
    fn locked_rejects_off_and_config_changes() {
        let mut machine = holding(0.4);
        send(&mut machine, Command::Lock, 1.0).unwrap();
        assert_eq!(
            send(&mut machine, Command::SetOn(false), 1.0),
            Err(Rejection::Locked)
        );
        let update = Command::UpdateConfig(Box::default());
        assert_eq!(send(&mut machine, update, 1.0), Err(Rejection::Locked));
        assert!(machine.on);
        assert_eq!(machine.config, test_config());
    }

    #[test]
    fn unlock_takes_the_full_30s_and_is_cancellable() {
        let mut machine = holding(0.4);
        send(&mut machine, Command::Lock, 0.0).unwrap();
        send(&mut machine, Command::BeginUnlock, 10.0).unwrap();

        machine.tick(39.9, reading(0.4));
        assert!(machine.locked);
        assert_eq!(
            send(&mut machine, Command::SetOn(false), 39.9),
            Err(Rejection::Locked)
        );
        assert!(
            (machine
                .snapshot(39.9, DeviceStatus::Connected)
                .unlock_remaining
                .unwrap()
                - 0.1)
                .abs()
                < 1e-9
        );

        send(&mut machine, Command::CancelUnlock, 39.9).unwrap();
        machine.tick(50.0, reading(0.4));
        assert!(machine.locked, "cancelled");

        send(&mut machine, Command::BeginUnlock, 50.0).unwrap();
        machine.tick(80.0, reading(0.4));
        assert!(!machine.locked);
        assert!(machine.on, "engine keeps running");
    }

    #[test]
    fn on_is_allowed_while_locked_and_restores_the_saved_base() {
        let state = PersistedState {
            config: test_config(),
            locked: true,
            level: 0.2,
            base: 0.4,
            on: true,
            ramp_running: true,
        };
        let mut machine = Machine::new(state);
        assert!(machine.on && machine.ramp_running);
        let write = machine.tick(0.0, reading(0.1)).unwrap();
        assert_eq!(write.volume, 0.4);
    }

    #[test]
    fn locked_restart_that_was_off_stays_off() {
        let state = PersistedState {
            config: test_config(),
            locked: true,
            base: 0.4,
            ..Default::default()
        };
        assert!(!Machine::new(state).on);
    }

    #[test]
    fn countdown_does_not_persist_but_the_lock_does() {
        let mut machine = holding(0.4);
        send(&mut machine, Command::Lock, 0.0).unwrap();
        send(&mut machine, Command::BeginUnlock, 1.0).unwrap();
        let json = serde_json::to_string(&machine.persisted()).unwrap();

        let restored = Machine::new(serde_json::from_str(&json).unwrap());
        assert!(restored.locked);
        assert_eq!(restored.base, 0.4);
        assert_eq!(restored.unlock_at, None);
    }

    #[test]
    fn unlocked_restart_comes_up_off_and_on_holds_the_saved_level() {
        let state = PersistedState {
            config: test_config(),
            locked: false,
            level: 0.4,
            base: 0.65,
            on: true,
            ramp_running: true,
        };
        let mut machine = Machine::new(state);
        assert!(!machine.on && !machine.ramp_running);
        send(&mut machine, Command::SetOn(true), 0.0).unwrap();
        let write = machine.tick(0.0, reading(0.7)).unwrap();
        assert_eq!(
            write.volume, 0.4,
            "a louder device is pulled to the set level"
        );
        assert_eq!(machine.base, 0.4);
    }

    #[test]
    fn set_level_sets_both_is_capped_and_is_refused_while_locked() {
        let mut config = test_config();
        config.ceiling = 0.7;
        let mut machine = Machine::new(PersistedState {
            config,
            ..PersistedState::default()
        });
        send(&mut machine, Command::SetLevel(2.0), 0.0).unwrap();
        assert_eq!((machine.level, machine.base), (0.7, 0.7));
        send(&mut machine, Command::Lock, 0.0).unwrap();
        assert_eq!(
            send(&mut machine, Command::SetLevel(0.1), 0.0),
            Err(Rejection::Locked)
        );
        assert_eq!((machine.level, machine.base), (0.7, 0.7));
    }

    /// A machine that is On at `level` with a ramp that took base up to 0.6.
    fn ramped_from(level: f64) -> Machine {
        let mut machine = Machine::new(PersistedState {
            config: test_config(),
            level,
            ..Default::default()
        });
        send(&mut machine, Command::SetOn(true), 0.0).unwrap();
        machine.tick(0.0, reading(level));
        machine.base = 0.6;
        machine
    }

    #[test]
    fn unlocked_off_returns_base_to_the_level() {
        let mut machine = ramped_from(0.3);
        send(&mut machine, Command::SetOn(false), 1.0).unwrap();
        assert_eq!(machine.base, 0.3);
        assert_eq!(machine.persisted().base, 0.3);
    }

    #[test]
    fn unlocked_off_with_no_level_goes_back_to_unset() {
        let mut machine = holding(0.45);
        send(&mut machine, Command::SetOn(false), 1.0).unwrap();
        assert_eq!(machine.base, 0.0);

        send(&mut machine, Command::SetOn(true), 2.0).unwrap();
        assert_eq!(
            machine.tick(2.0, reading(0.2)),
            None,
            "adopted, not written"
        );
        assert_eq!((machine.level, machine.base), (0.0, 0.2));
    }

    #[test]
    fn a_shutdown_while_locked_keeps_the_live_base() {
        let mut machine = ramped_from(0.3);
        send(&mut machine, Command::Lock, 1.0).unwrap();
        machine.shutdown(1.0);
        assert_eq!(machine.base, 0.6);
    }

    #[test]
    fn unlocking_while_off_returns_base_to_the_level() {
        let mut machine = Machine::new(PersistedState {
            config: test_config(),
            locked: true,
            level: 0.3,
            base: 0.6,
            ..Default::default()
        });
        assert_eq!(machine.base, 0.6, "locked keeps the live base");
        send(&mut machine, Command::BeginUnlock, 0.0).unwrap();
        machine.tick(30.0, reading(0.1));
        assert!(!machine.locked);
        assert_eq!(machine.base, 0.3);
    }

    #[test]
    fn unlocking_while_on_keeps_the_live_base_until_off() {
        let mut machine = ramped_from(0.3);
        send(&mut machine, Command::Lock, 0.0).unwrap();
        send(&mut machine, Command::BeginUnlock, 0.0).unwrap();
        machine.tick(30.0, reading(0.6));
        assert!(!machine.locked);
        assert_eq!(machine.base, 0.6);
    }

    #[test]
    fn dragging_the_ceiling_down_and_back_loses_nothing() {
        let mut machine = Machine::new(PersistedState {
            config: test_config(),
            level: 0.6,
            ..Default::default()
        });
        let with_ceiling = |ceiling| {
            Command::UpdateConfig(Box::new(Config {
                ceiling,
                ..test_config()
            }))
        };

        send(&mut machine, with_ceiling(0.2), 0.0).unwrap();
        assert_eq!(machine.base, 0.2, "used clamped");
        assert_eq!(machine.persisted().level, 0.6);
        assert_eq!(machine.persisted().config.ramp.target, 0.8);

        send(&mut machine, with_ceiling(1.0), 0.0).unwrap();
        assert_eq!(machine.base, 0.6);
        assert_eq!(machine.persisted().config.ramp.target, 0.8);
    }

    #[test]
    fn punished_press_waits_out_its_delay_before_the_spike() {
        let mut config = test_config();
        config.begging.delay_min_secs = 2.0;
        config.begging.delay_max_secs = 2.0;
        config.begging.spike_min_secs = 1.0;
        config.begging.spike_max_secs = 1.0;
        let mut machine = holding_with(config, 0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();

        assert_eq!(
            machine.tick(2.9, reading(0.4)),
            None,
            "no change during the delay"
        );
        assert!(machine.snapshot(2.9, DeviceStatus::Connected).pending);
        assert_eq!(
            send(&mut machine, Command::Press(EventKind::Begging), 2.9),
            Err(Rejection::EventActive)
        );

        let write = machine.tick(3.0, reading(0.4)).unwrap();
        assert!(write.volume > 0.4, "spike starts when the delay ends");
        assert_eq!(machine.stage, Stage::Spike);
        machine.tick(4.0, reading(write.volume));
        assert_eq!(
            machine.stage,
            Stage::Recovery,
            "spike keeps its full length"
        );
    }

    #[test]
    fn punished_full_drop_reaches_silence_even_on_a_late_tick() {
        let mut config = test_config();
        config.begging.drop = 1.0;
        config.begging.spike_min_secs = 1.0;
        config.begging.spike_max_secs = 1.0;
        let mut machine = holding_with(config, 0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        let spike = machine.tick(1.0, reading(0.4)).unwrap();

        let write = machine.tick(2.015, reading(spike.volume)).unwrap();
        assert_eq!(write.volume, 0.0);
    }

    #[test]
    fn a_fresh_countdown_never_shows_more_than_the_full_time() {
        let mut machine = holding(0.4);
        send(&mut machine, Command::Lock, 0.0).unwrap();
        let now = 0.1 + 0.2;
        send(&mut machine, Command::BeginUnlock, now).unwrap();
        let remaining = machine
            .snapshot(now, DeviceStatus::Connected)
            .unlock_remaining;
        assert!(remaining.is_some_and(|secs| secs <= UNLOCK_SECS));
    }

    #[test]
    fn panic_silences_and_turns_off_through_the_lock() {
        let mut config = test_config();
        config.panic_hotkey = true;
        let mut machine = holding_with(config, 0.4);
        send(&mut machine, Command::Lock, 0.0).unwrap();

        send(&mut machine, Command::Panic, 1.0).unwrap();
        let write = machine.tick(1.0, reading(0.4)).unwrap();
        assert_eq!(write.volume, 0.0);
        assert!(!machine.on && machine.locked);
        assert!(!machine.persisted().on, "a restart must not resume it");
        assert_eq!(
            machine.tick(1.02, reading(0.0)),
            None,
            "silence is confirmed"
        );
    }

    #[test]
    fn panic_silences_a_device_that_was_already_off() {
        let mut config = test_config();
        config.panic_hotkey = true;
        let mut machine = holding_with(config, 0.4);
        send(&mut machine, Command::SetOn(false), 0.5).unwrap();
        machine.tick(0.6, reading(0.4));

        send(&mut machine, Command::Panic, 1.0).unwrap();
        assert_eq!(machine.tick(1.0, reading(0.4)).unwrap().volume, 0.0);
    }

    #[test]
    fn panic_is_refused_unless_enabled_and_cannot_be_enabled_while_locked() {
        let mut machine = holding(0.4);
        send(&mut machine, Command::Lock, 0.0).unwrap();
        assert_eq!(
            send(&mut machine, Command::Panic, 1.0),
            Err(Rejection::PanicDisabled)
        );
        assert!(machine.on);

        let mut config = machine.config().clone();
        config.panic_hotkey = true;
        assert_eq!(
            send(&mut machine, Command::UpdateConfig(Box::new(config)), 1.0),
            Err(Rejection::Locked)
        );
    }

    #[test]
    fn spared_press_skips_the_delay() {
        let mut config = test_config();
        config.begging.delay_min_secs = 5.0;
        config.begging.delay_max_secs = 5.0;
        let mut machine = holding_with(config, 0.4);
        press(&mut machine, EventKind::Begging, SPARE, 1.0).unwrap();
        machine.tick(1.0, reading(0.4));
        assert_eq!(machine.stage, Stage::Recovery);
    }

    #[test]
    fn off_leaves_the_device_alone_but_never_stuck_on_a_spike() {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        machine.tick(1.0, reading(0.4)).unwrap();
        send(&mut machine, Command::SetOn(false), 1.1).unwrap();

        assert_eq!(machine.tick(1.2, reading(0.5)).unwrap().volume, 0.4);
        assert_eq!(machine.tick(1.25, reading(0.4)), None, "confirmed");
        assert_eq!(machine.tick(1.3, reading(0.9)), None);
        assert_eq!(
            machine.tick(
                1.4,
                Some(DeviceReading {
                    volume: 0.1,
                    muted: true
                })
            ),
            None
        );
    }

    fn off_during_a_spike() -> Machine {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();
        machine.tick(1.0, reading(0.4)).unwrap();
        send(&mut machine, Command::SetOn(false), 1.1).unwrap();
        machine
    }

    #[test]
    fn a_settle_write_that_failed_is_retried_when_the_device_returns() {
        let mut machine = off_during_a_spike();
        assert_eq!(machine.tick(1.12, reading(0.5)).unwrap().volume, 0.4);
        machine.tick(1.14, None);
        assert!(machine.tick(2.2, reading(0.5)).is_some());
    }

    #[test]
    fn a_device_that_rounds_the_settle_up_is_stepped_down_and_then_left_alone() {
        let mut machine = off_during_a_spike();
        let mut device = Rounding::new(0.5, 0.15);
        drive(&mut machine, &mut device, 1.12, 2.0);
        assert!(device.value() <= 0.4, "device at {}", device.value());
        assert_eq!(machine.settle.map(|settle| settle.level), None);
        assert_eq!(
            machine.tick(2.0, reading(0.9)),
            None,
            "the slider is free again"
        );
    }

    #[test]
    fn changing_device_drops_a_pending_settle() {
        let mut machine = off_during_a_spike();
        let mut config = test_config();
        config.device_id = Some("other".into());
        send(&mut machine, Command::UpdateConfig(Box::new(config)), 1.2).unwrap();
        assert_eq!(machine.tick(1.3, reading(0.9)), None);
    }

    #[test]
    fn spike_size_and_duration_stay_in_range() {
        let config = EventConfig {
            chance: 1.0,
            spike_min: 0.1,
            spike_max: 0.3,
            spike_min_secs: 2.0,
            spike_max_secs: 5.0,
            ..EventConfig::default()
        };
        let mut rng = fastrand::Rng::with_seed(7);
        for _ in 0..2000 {
            let spike = resolve_press(&config, &mut rng).spike.unwrap();
            assert!((1.1..1.3).contains(&spike.gain), "{spike:?}");
            assert!((2.0..5.0).contains(&spike.secs), "{spike:?}");
        }
    }

    #[test]
    fn chance_decides_punishment() {
        let config = EventConfig {
            chance: 0.5,
            ..EventConfig::default()
        };
        assert!(resolve_press(&config, &mut Fixed(0.49)).spike.is_some());
        let spared = resolve_press(&config, &mut Fixed(0.5));
        assert_eq!(
            spared,
            PressPlan {
                spike: None,
                drop: config.drop
            }
        );
    }

    #[test]
    fn punished_press_spikes_then_drops_then_recovers_to_base() {
        let mut machine = holding(0.4);
        let mut device = 0.4;
        press(&mut machine, EventKind::Begging, PUNISH, 1.0).unwrap();

        assert!(
            (play(&mut machine, 1.0, &mut device) - 0.5).abs() < 1e-9,
            "+25% of 40 is 50"
        );
        assert_eq!(machine.stage, Stage::Spike);
        assert!(
            (play(&mut machine, 3.0, &mut device) - 0.2).abs() < 1e-9,
            "50% drop"
        );
        assert_eq!(machine.stage, Stage::Recovery);
        assert!((play(&mut machine, 13.0, &mut device) - 0.4).abs() < 1e-9);
        assert_eq!(machine.stage, Stage::Hold);
        assert_eq!(machine.event.map(|e| e.kind), None);
    }

    #[test]
    fn presses_during_an_active_event_are_ignored() {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, SPARE, 1.0).unwrap();
        machine.tick(5.0, reading(0.4));
        let before = machine.event;

        let result = press(&mut machine, EventKind::Begging, SPARE, 5.0);
        assert_eq!(result, Err(Rejection::EventActive));
        assert_eq!(machine.event, before, "recovery was not restarted");
    }

    #[test]
    fn came_pre_empts_begging_but_not_the_reverse() {
        let mut machine = holding(0.4);
        press(&mut machine, EventKind::Begging, SPARE, 1.0).unwrap();
        press(&mut machine, EventKind::Came, SPARE, 2.0).unwrap();
        assert_eq!(machine.event.map(|e| e.kind), Some(EventKind::Came));

        let result = press(&mut machine, EventKind::Begging, SPARE, 3.0);
        assert_eq!(result, Err(Rejection::EventActive));
        let result = press(&mut machine, EventKind::Came, SPARE, 3.0);
        assert_eq!(result, Err(Rejection::EventActive));
    }

    #[test]
    fn cooldown_runs_from_the_end_of_the_event() {
        let mut config = test_config();
        config.begging.cooldown_secs = 5.0;
        let mut machine = holding_with(config, 0.4);
        press(&mut machine, EventKind::Begging, SPARE, 0.0).unwrap();
        machine.tick(10.0, reading(0.4));
        assert_eq!(machine.event.map(|e| e.kind), None);

        let result = press(&mut machine, EventKind::Begging, SPARE, 14.9);
        assert_eq!(result, Err(Rejection::CoolingDown));
        press(&mut machine, EventKind::Begging, SPARE, 15.0).unwrap();
    }

    #[test]
    fn buttons_do_nothing_while_off() {
        let mut machine = Machine::new(PersistedState::default());
        let press = Command::Press(EventKind::Came);
        assert_eq!(send(&mut machine, press, 0.0), Err(Rejection::Off));
        assert_eq!(
            send(&mut machine, Command::ToggleRamp, 0.0),
            Err(Rejection::Off)
        );
    }

    #[test]
    fn ramp_stops_at_target_and_pause_keeps_base() {
        let mut machine = holding(0.7);
        let mut device = 0.7;
        send(&mut machine, Command::ToggleRamp, 0.0).unwrap();

        // 0.6 per minute is 0.01 per second.
        for step in 1..=25 {
            play(&mut machine, f64::from(step) * 0.2, &mut device);
        }
        assert!((machine.base - 0.75).abs() < 1e-9);

        send(&mut machine, Command::ToggleRamp, 5.0).unwrap();
        play(&mut machine, 5.2, &mut device);
        play(&mut machine, 5.4, &mut device);
        assert!((machine.base - 0.75).abs() < 1e-9, "paused, not reset");

        send(&mut machine, Command::ToggleRamp, 5.4).unwrap();
        for step in 28..=100 {
            play(&mut machine, f64::from(step) * 0.2, &mut device);
        }
        assert_eq!(machine.base, 0.8);
        assert!(!machine.ramp_running);
    }

    #[test]
    fn ramp_target_is_capped_at_ceiling() {
        let mut config = test_config();
        config.ceiling = 0.72;
        let mut machine = holding_with(config, 0.7);
        let mut device = 0.7;
        send(&mut machine, Command::ToggleRamp, 0.0).unwrap();
        for step in 1..=50 {
            play(&mut machine, f64::from(step) * 0.2, &mut device);
        }
        assert_eq!(machine.base, 0.72);
    }

    #[test]
    fn ramp_continues_during_an_event() {
        let mut machine = holding(0.4);
        send(&mut machine, Command::ToggleRamp, 0.0).unwrap();
        press(&mut machine, EventKind::Begging, SPARE, 0.0).unwrap();
        let mut device = 0.4;
        for step in 1..=5 {
            play(&mut machine, f64::from(step) * 0.2, &mut device);
        }
        assert!((machine.base - 0.41).abs() < 1e-9);
        assert_eq!(machine.stage, Stage::Recovery, "event stage beats ramp");
    }

    #[test]
    fn ramp_does_not_jump_after_a_long_gap() {
        let mut machine = holding(0.4);
        send(&mut machine, Command::ToggleRamp, 0.0).unwrap();
        machine.tick(8.0 * 3600.0, reading(0.4));
        assert!(machine.base < 0.41, "got {}", machine.base);
    }

    #[test]
    fn stage_effect_dips_output_from_stage_start() {
        let mut config = test_config();
        config.effects.hold = Effect {
            wave: Wave::Square,
            hz: 1.0,
            depth: 0.5,
        };
        let mut machine = holding_with(config, 0.4);
        let mut device = 0.4;
        assert!((play(&mut machine, 0.25, &mut device) - 0.4).abs() < 1e-9);
        assert!((play(&mut machine, 0.75, &mut device) - 0.2).abs() < 1e-9);
        assert_eq!(machine.base, 0.4, "a dip is not a decrease of base");
    }

    #[test]
    fn device_cannot_change_while_on() {
        let mut machine = holding(0.4);
        let mut config = test_config();
        config.device_id = Some("other".into());
        let update = Command::UpdateConfig(Box::new(config));
        assert_eq!(
            send(&mut machine, update, 1.0),
            Err(Rejection::DeviceChangeWhileOn)
        );
    }
}
