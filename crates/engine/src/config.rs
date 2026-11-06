//! User configuration and the state that persists across restarts.
//!
//! Levels and proportions are fractions: 0.4 is 40 on the Windows slider. Serialized camelCase.

use serde::{Deserialize, Serialize};

use crate::wave::Effect;

/// Runner tick rate. `examples/probe.rs` on Steam Streaming Speakers, 5 runs of 500 writes:
/// `set_volume` median 62-77us, p99 0.2-1.2ms, max 5.9ms. Reads match, max 12.7ms. A read
/// right after a write was never stale. `sleep(20ms)` took a median 20.6ms, p99 31-56ms.
///
/// A busy tick is 3 reads and a write: about 0.3ms, under 2% of a 20ms tick. So the device
/// is not the limit, the scheduler is, and ticking faster only adds late ticks. Nor would
/// it be audible: the Windows audio engine applies volume once per period, typically 10ms,
/// and 50 Hz gives every write a period of its own. Endpoints with hardware volume were not
/// measured and may be much slower. A slow one skips ticks, which is safe but coarser.
pub const TICK_HZ: u32 = 50;

/// Highest effect rate: 5 ticks per cycle at `TICK_HZ`, the fewest that still read as the
/// chosen wave. It also keeps a p99 late tick (30ms or so) under a third of a cycle, so one
/// late tick bends a cycle rather than skipping it.
pub const MAX_EFFECT_HZ: f64 = 10.0;

const MAX_SPIKE: f64 = 10.0;
const MAX_SECS: f64 = 3600.0;
const MAX_CURVE_K: f64 = 1000.0;

/// Clamps to `min..=max`. NaN becomes `min`, the quiet end of every range here.
pub(crate) fn clamped(value: f64, min: f64, max: f64) -> f64 {
    if value.is_nan() {
        min
    } else {
        value.clamp(min, max)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Endpoint id. `None` means no device: there is deliberately no default-device fallback.
    pub device_id: Option<String>,
    /// Hard maximum. Nothing exceeds it, including external raises. Levels above it are
    /// clamped where they are used, not stored lower.
    pub ceiling: f64,
    pub ramp: RampConfig,
    pub begging: EventConfig,
    pub came: EventConfig,
    pub effects: StageEffects,
    /// Allows `Command::Panic`, the one command that ignores the lock. Off by default, and
    /// frozen while locked like all config, so it cannot be turned on to escape a lock.
    pub panic_hotkey: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_id: None,
            ceiling: 0.7,
            ramp: RampConfig::default(),
            begging: EventConfig::default(),
            came: EventConfig {
                spike_min: 0.25,
                spike_max: 0.5,
                spike_min_secs: 2.0,
                spike_max_secs: 6.0,
                recover_secs: 90.0,
                curve_k: 5.0,
                ..EventConfig::default()
            },
            effects: StageEffects::default(),
            panic_hotkey: false,
        }
    }
}

impl Config {
    /// Forces every value into range, so the machine can trust a config from anywhere.
    pub fn sanitized(mut self) -> Self {
        self.ceiling = clamped(self.ceiling, 0.0, 1.0);
        self.ramp.rate = clamped(self.ramp.rate, 0.0, 1.0);
        self.ramp.target = clamped(self.ramp.target, 0.0, 1.0);
        self.begging = self.begging.sanitized();
        self.came = self.came.sanitized();
        for effect in self.effects.all_mut() {
            effect.hz = clamped(effect.hz, 0.0, MAX_EFFECT_HZ);
            effect.depth = clamped(effect.depth, 0.0, 1.0);
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RampConfig {
    /// Rise per minute in slider points: 0.01 is 1 point a minute, whatever the base is.
    pub rate: f64,
    /// Level the ramp stops at, or the ceiling if that is lower.
    pub target: f64,
}

impl Default for RampConfig {
    fn default() -> Self {
        Self {
            rate: 0.01,
            target: 0.5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EventConfig {
    /// Probability that a press is punished with a spike.
    pub chance: f64,
    /// Spike size relative to base: 0.25 on a base of 0.4 gives 0.5.
    pub spike_min: f64,
    pub spike_max: f64,
    pub spike_min_secs: f64,
    pub spike_max_secs: f64,
    /// Random wait between a punished press and its spike. Spared presses do not wait.
    pub delay_min_secs: f64,
    pub delay_max_secs: f64,
    /// Proportion of the level removed: 1.0 is silence.
    pub drop: f64,
    pub recover_secs: f64,
    /// Recovery curve steepness, `k` in the spec. Higher rises faster at first.
    pub curve_k: f64,
    /// Wait after the event ends before it can run again. 0 is none.
    pub cooldown_secs: f64,
}

impl Default for EventConfig {
    fn default() -> Self {
        Self {
            chance: 0.5,
            spike_min: 0.1,
            spike_max: 0.25,
            spike_min_secs: 0.5,
            spike_max_secs: 3.0,
            delay_min_secs: 0.0,
            delay_max_secs: 0.0,
            drop: 1.0,
            recover_secs: 30.0,
            curve_k: 20.0,
            cooldown_secs: 0.0,
        }
    }
}

impl EventConfig {
    fn sanitized(self) -> Self {
        let spike_min = clamped(self.spike_min, 0.0, MAX_SPIKE);
        let spike_min_secs = clamped(self.spike_min_secs, 0.0, MAX_SECS);
        let delay_min_secs = clamped(self.delay_min_secs, 0.0, MAX_SECS);
        Self {
            chance: clamped(self.chance, 0.0, 1.0),
            spike_min,
            spike_max: clamped(self.spike_max, spike_min, MAX_SPIKE),
            spike_min_secs,
            spike_max_secs: clamped(self.spike_max_secs, spike_min_secs, MAX_SECS),
            delay_min_secs,
            delay_max_secs: clamped(self.delay_max_secs, delay_min_secs, MAX_SECS),
            drop: clamped(self.drop, 0.0, 1.0),
            recover_secs: clamped(self.recover_secs, 0.0, MAX_SECS),
            curve_k: clamped(self.curve_k, 0.0, MAX_CURVE_K),
            cooldown_secs: clamped(self.cooldown_secs, 0.0, MAX_SECS),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StageEffects {
    pub hold: Effect,
    pub ramp: Effect,
    pub spike: Effect,
    pub recovery: Effect,
}

impl StageEffects {
    fn all_mut(&mut self) -> [&mut Effect; 4] {
        [
            &mut self.hold,
            &mut self.ramp,
            &mut self.spike,
            &mut self.recovery,
        ]
    }
}

/// What the shell saves as JSON. `on` and `ramp_running` only resume while locked, so a
/// restart cannot get around the lock, and an unlocked restart never drives the device.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PersistedState {
    pub config: Config,
    pub locked: bool,
    /// The set level, where an unlocked session starts and returns to. 0 means unset: On
    /// adopts the device's level as `base` and this stays 0.
    pub level: f64,
    /// The live held level. Only a locked restart resumes it.
    pub base: f64,
    pub on: bool,
    pub ramp_running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wave::Wave;

    #[test]
    fn sanitize_clamps_every_kind_of_bad_value() {
        let mut config = Config {
            ceiling: f64::NAN,
            ..Config::default()
        };
        config.ramp.target = 1.5;
        config.begging.chance = 7.0;
        config.begging.spike_min = 0.5;
        config.begging.spike_max = 0.1;
        config.effects.hold = Effect {
            wave: Wave::Sine,
            hz: 500.0,
            depth: -1.0,
        };

        let config = config.sanitized();
        assert_eq!(config.ceiling, 0.0, "NaN ceiling must fail quiet");
        assert_eq!(config.ramp.target, 1.0, "target is not cut by the ceiling");
        assert_eq!(config.begging.chance, 1.0);
        assert_eq!(config.begging.spike_max, 0.5, "max follows min");
        assert_eq!(config.effects.hold.hz, MAX_EFFECT_HZ);
        assert_eq!(config.effects.hold.depth, 0.0);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let state: PersistedState = serde_json::from_str(r#"{"locked":true}"#).unwrap();
        assert!(state.locked);
        assert_eq!(state.config, Config::default());
    }

    #[test]
    fn defaults_are_already_sane() {
        assert_eq!(Config::default().sanitized(), Config::default());
    }

    #[test]
    fn defaults_match_the_owners_numbers() {
        let config = Config::default();
        assert_eq!(config.ceiling, 0.7);
        assert_eq!(
            config.ramp,
            RampConfig {
                rate: 0.01,
                target: 0.5
            }
        );
        let event = |spike: (f64, f64), secs: (f64, f64), recover_secs, curve_k| EventConfig {
            chance: 0.5,
            spike_min: spike.0,
            spike_max: spike.1,
            spike_min_secs: secs.0,
            spike_max_secs: secs.1,
            delay_min_secs: 0.0,
            delay_max_secs: 0.0,
            drop: 1.0,
            recover_secs,
            curve_k,
            cooldown_secs: 0.0,
        };
        assert_eq!(config.begging, event((0.1, 0.25), (0.5, 3.0), 30.0, 20.0));
        assert_eq!(config.came, event((0.25, 0.5), (2.0, 6.0), 90.0, 5.0));
    }
}
