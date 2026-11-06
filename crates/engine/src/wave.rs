//! LFO effects. An effect only dips the level, it never adds to it.

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Wave {
    #[default]
    None,
    Sine,
    Saw,
    Square,
}

impl Wave {
    /// Value in 0..=1 at `t` seconds into a stage. Sine and square start at 1, so entering a
    /// stage does not jump. Saw rises, so its sudden edge is a drop rather than a jolt upward.
    pub fn sample(self, hz: f64, t: f64) -> f64 {
        let phase = (hz * t).rem_euclid(1.0);
        match self {
            Wave::None => 1.0,
            Wave::Sine => 0.5 + 0.5 * (TAU * phase).cos(),
            Wave::Saw => phase,
            Wave::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Effect {
    pub wave: Wave,
    pub hz: f64,
    pub depth: f64,
}

impl Effect {
    /// Gain in 0..=1 at `t` seconds into a stage.
    pub fn gain(&self, t: f64) -> f64 {
        1.0 - self.depth * (1.0 - self.wave.sample(self.hz, t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAVES: [Wave; 4] = [Wave::None, Wave::Sine, Wave::Saw, Wave::Square];

    fn times() -> impl Iterator<Item = f64> {
        (0..5000).map(|step| f64::from(step) * 0.0137)
    }

    #[test]
    fn waves_stay_in_range() {
        for wave in WAVES {
            for t in times() {
                let value = wave.sample(3.7, t);
                assert!((0.0..=1.0).contains(&value), "{wave:?} at {t}: {value}");
            }
        }
    }

    #[test]
    fn effect_never_exceeds_one() {
        for wave in WAVES {
            for depth in [0.0, 0.3, 1.0] {
                let effect = Effect {
                    wave,
                    hz: 3.7,
                    depth,
                };
                for t in times() {
                    let gain = effect.gain(t);
                    assert!((0.0..=1.0).contains(&gain), "{effect:?} at {t}: {gain}");
                }
            }
        }
    }

    #[test]
    fn no_effect_is_unity() {
        assert_eq!(Effect::default().gain(1.3), 1.0);
    }
}
