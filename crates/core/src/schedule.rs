//! Random variation of click intervals and positions.

use crate::config::{ClickSettings, JITTER_TRUNCATION_SIGMAS};
use std::time::Duration;

/// Draws intervals and position offsets from a seeded RNG, so runs are
/// reproducible in tests.
#[derive(Debug)]
pub struct Scheduler {
    rng: fastrand::Rng,
}

impl Scheduler {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: fastrand::Rng::with_seed(seed),
        }
    }

    /// Next start-to-start interval: the nominal interval plus Gaussian jitter
    /// truncated at ±3σ (resampled, so the distribution has no spikes at the
    /// bounds).
    pub fn interval(&mut self, settings: &ClickSettings) -> Duration {
        let sigma = settings.jitter_sigma_ms;
        if sigma == 0.0 {
            return settings.interval;
        }
        let z = loop {
            let z = self.standard_normal();
            if z.abs() <= JITTER_TRUNCATION_SIGMAS {
                break z;
            }
        };
        let ms = settings.interval.as_secs_f64() * 1000.0 + z * sigma;
        Duration::from_secs_f64(ms / 1000.0)
    }

    /// Pixel offset within a circle of `radius` (Gaussian with σ = radius / 2,
    /// rounded to whole pixels and resampled if the rounded point lies outside).
    pub fn position_offset(&mut self, radius: u32) -> (i32, i32) {
        if radius == 0 {
            return (0, 0);
        }
        let r = f64::from(radius);
        let sigma = r / 2.0;
        let r2 = i64::from(radius) * i64::from(radius);
        loop {
            // Values are bounded by the resampling check below, so the casts cannot overflow.
            let dx = (self.standard_normal() * sigma).round() as i64;
            let dy = (self.standard_normal() * sigma).round() as i64;
            if dx * dx + dy * dy <= r2 {
                return (dx as i32, dy as i32);
            }
        }
    }

    /// Box–Muller transform (one value per call, for simplicity).
    fn standard_normal(&mut self) -> f64 {
        let u1 = 1.0 - self.rng.f64(); // (0, 1], avoids ln(0)
        let u2 = self.rng.f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClickConfig;

    fn settings(interval_ms: u32, sigma: f64) -> ClickSettings {
        ClickConfig {
            interval_ms,
            interval_jitter_sigma_ms: sigma,
            hold_ms: 0,
            ..ClickConfig::default()
        }
        .validated()
        .unwrap()
    }

    fn mean_std(xs: &[f64]) -> (f64, f64) {
        let n = xs.len() as f64;
        let mean = xs.iter().sum::<f64>() / n;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        (mean, var.sqrt())
    }

    #[test]
    fn zero_jitter_is_exact() {
        let mut s = Scheduler::new(1);
        let st = settings(100, 0.0);
        for _ in 0..100 {
            assert_eq!(s.interval(&st), Duration::from_millis(100));
        }
    }

    #[test]
    fn interval_jitter_is_truncated_gaussian_around_the_nominal_value() {
        let mut s = Scheduler::new(42);
        let st = settings(100, 10.0);
        let xs: Vec<f64> = (0..20_000)
            .map(|_| s.interval(&st).as_secs_f64() * 1000.0)
            .collect();
        assert!(xs.iter().all(|&x| (70.0..=130.0).contains(&x)));
        let (mean, std) = mean_std(&xs);
        assert!((mean - 100.0).abs() < 0.3, "mean {mean}");
        // Truncation at ±3σ reduces the standard deviation by about 1.4 %.
        assert!((std - 9.87).abs() < 0.25, "std {std}");
        // Both tails are actually reached.
        assert!(xs.iter().any(|&x| x < 75.0) && xs.iter().any(|&x| x > 125.0));
    }

    #[test]
    fn same_seed_gives_the_same_sequence() {
        let st = settings(100, 10.0);
        let mut a = Scheduler::new(7);
        let mut b = Scheduler::new(7);
        let mut c = Scheduler::new(8);
        let sa: Vec<_> = (0..50).map(|_| a.interval(&st)).collect();
        let sb: Vec<_> = (0..50).map(|_| b.interval(&st)).collect();
        let sc: Vec<_> = (0..50).map(|_| c.interval(&st)).collect();
        assert_eq!(sa, sb);
        assert_ne!(sa, sc);
    }

    #[test]
    fn position_offsets_stay_within_the_radius_and_are_unbiased() {
        let mut s = Scheduler::new(3);
        let pts: Vec<(i32, i32)> = (0..20_000).map(|_| s.position_offset(3)).collect();
        assert!(pts.iter().all(|&(x, y)| x * x + y * y <= 9));
        let xs: Vec<f64> = pts.iter().map(|p| f64::from(p.0)).collect();
        let ys: Vec<f64> = pts.iter().map(|p| f64::from(p.1)).collect();
        let (mx, sx) = mean_std(&xs);
        let (my, _) = mean_std(&ys);
        assert!(mx.abs() < 0.05 && my.abs() < 0.05, "means {mx} {my}");
        assert!(sx > 1.2 && sx < 1.6, "std {sx}");
        // Rounding (not truncation): the zero bin is not inflated. With σ = 1.5
        // about 26 % of rounded samples are 0; truncation toward zero gives ~50 %.
        let zeros = xs.iter().filter(|&&x| x == 0.0).count() as f64 / xs.len() as f64;
        assert!(zeros > 0.22 && zeros < 0.32, "zero fraction {zeros}");
        // The full radius is reachable on both sides.
        assert!(pts.iter().any(|p| p.0 == 3) && pts.iter().any(|p| p.0 == -3));
    }

    #[test]
    fn zero_radius_never_moves() {
        let mut s = Scheduler::new(9);
        assert!((0..100).all(|_| s.position_offset(0) == (0, 0)));
    }
}
