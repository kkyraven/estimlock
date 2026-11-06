//! Recovery curve for the drop step of an event.

/// Gain at progress `p` (0..=1) of a recovery from `drop` (0..=1) with steepness `k`.
/// Starts at `1 - drop`, ends at 1, and rises fastest at the start.
pub fn recovery_gain(p: f64, drop: f64, k: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    // The curve tends to a straight line as k approaches 0, where the formula itself is 0/0.
    let shaped = if k > 1e-9 {
        (k * p).ln_1p() / k.ln_1p()
    } else {
        p
    };
    (1.0 - drop) + drop * shaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_exact() {
        assert!((recovery_gain(0.0, 0.6, 20.0) - 0.4).abs() < 1e-12);
        assert!((recovery_gain(1.0, 0.6, 20.0) - 1.0).abs() < 1e-12);
        assert_eq!(recovery_gain(0.0, 1.0, 20.0), 0.0);
    }

    #[test]
    fn rises_fast_first() {
        let first_half = recovery_gain(0.5, 1.0, 20.0);
        assert!(first_half > 0.5, "got {first_half}");
        let early = recovery_gain(0.1, 1.0, 20.0) - recovery_gain(0.0, 1.0, 20.0);
        let late = recovery_gain(1.0, 1.0, 20.0) - recovery_gain(0.9, 1.0, 20.0);
        assert!(early > late * 3.0, "early {early}, late {late}");
    }

    #[test]
    fn zero_steepness_is_linear() {
        assert!((recovery_gain(0.25, 1.0, 0.0) - 0.25).abs() < 1e-12);
    }
}
