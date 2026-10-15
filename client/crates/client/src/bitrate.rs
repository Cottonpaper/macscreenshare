//! Adaptive-bitrate controller.
//!
//! A small additive-increase / multiplicative-decrease (AIMD) controller that
//! maps observed packet loss to a target bitrate. The session feeds it one
//! measurement per window and forwards any change to the host as `set_bitrate`.

/// Tunables for [`BitrateController`]. Kept as a struct so tests can pick
/// realistic-but-fast parameters.
#[derive(Debug, Clone, Copy)]
pub struct BitrateConfig {
    pub min_kbps: u32,
    pub max_kbps: u32,
    /// Start decreasing above this loss ratio.
    pub loss_high: f64,
    /// Treat the link as clean at or below this loss ratio.
    pub loss_low: f64,
    /// Multiplicative decrease applied on high loss.
    pub decrease: f64,
    /// Additive increase applied after a run of clean windows.
    pub increase: f64,
    /// Consecutive clean windows required before increasing.
    pub clean_windows: u32,
    /// Ignore changes smaller than this fraction of the current rate.
    pub hysteresis: f64,
}

impl BitrateConfig {
    /// Defaults with `max_kbps` as the cap (the client's `--bitrate`).
    pub fn for_cap(max_kbps: u32) -> Self {
        BitrateConfig {
            min_kbps: 1_000,
            max_kbps: max_kbps.max(1_000),
            loss_high: 0.02,
            loss_low: 0.005,
            decrease: 0.75,
            increase: 1.10,
            clean_windows: 5,
            hysteresis: 0.08,
        }
    }
}

/// AIMD bitrate controller. Starts at the cap (the requested max).
#[derive(Debug, Clone)]
pub struct BitrateController {
    cfg: BitrateConfig,
    current: u32,
    clean_streak: u32,
}

impl BitrateController {
    pub fn new(cfg: BitrateConfig) -> Self {
        Self {
            current: cfg.max_kbps,
            cfg,
            clean_streak: 0,
        }
    }

    /// The bitrate currently in effect.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// Feed one measurement window: `expected` frames the host sent, `lost`
    /// frames the client failed to receive. Returns the new bitrate when it
    /// changed enough to be worth telling the host about.
    pub fn observe(&mut self, expected: u64, lost: u64) -> Option<u32> {
        if expected == 0 {
            return None;
        }
        let loss = lost as f64 / expected as f64;

        let candidate = if loss > self.cfg.loss_high {
            self.clean_streak = 0;
            (self.current as f64 * self.cfg.decrease).round() as u32
        } else if loss <= self.cfg.loss_low {
            self.clean_streak += 1;
            if self.clean_streak >= self.cfg.clean_windows {
                self.clean_streak = 0;
                (self.current as f64 * self.cfg.increase).round() as u32
            } else {
                self.current
            }
        } else {
            // Between the thresholds: hold, and don't count it as clean.
            self.clean_streak = 0;
            self.current
        };

        let candidate = candidate.clamp(self.cfg.min_kbps, self.cfg.max_kbps);
        if candidate == self.current {
            return None;
        }
        // Changes that land exactly on the floor or cap always apply; other
        // changes need to clear the hysteresis band to avoid churn.
        let at_bound = candidate == self.cfg.min_kbps || candidate == self.cfg.max_kbps;
        if !at_bound {
            let delta = (candidate as f64 - self.current as f64).abs() / self.current as f64;
            if delta < self.cfg.hysteresis {
                return None;
            }
        }
        self.current = candidate;
        Some(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BitrateConfig {
        BitrateConfig {
            min_kbps: 1_000,
            max_kbps: 10_000,
            ..BitrateConfig::for_cap(10_000)
        }
    }

    #[test]
    fn starts_at_the_cap() {
        let c = BitrateController::new(cfg());
        assert_eq!(c.current(), 10_000);
    }

    #[test]
    fn high_loss_decreases_multiplicatively() {
        let mut c = BitrateController::new(cfg());
        // 10% loss -> below the cap.
        assert_eq!(c.observe(100, 10), Some(7_500));
        assert_eq!(c.current(), 7_500);
        // And again.
        assert_eq!(c.observe(100, 10), Some(5_625));
    }

    #[test]
    fn decrease_floors_at_minimum() {
        let mut c = BitrateController::new(cfg());
        for _ in 0..20 {
            c.observe(100, 50);
        }
        assert_eq!(c.current(), 1_000);
    }

    #[test]
    fn clean_windows_increase_after_a_streak() {
        let mut c = BitrateController::new(BitrateConfig {
            min_kbps: 1_000,
            max_kbps: 10_000,
            ..cfg()
        });
        // Drop once so there is headroom to climb.
        assert_eq!(c.observe(100, 10), Some(7_500));
        // Four clean windows: no change yet (streak needs 5).
        for _ in 0..4 {
            assert_eq!(c.observe(100, 0), None);
        }
        // Fifth clean window increases by 10%.
        assert_eq!(c.observe(100, 0), Some(8_250));
    }

    #[test]
    fn increase_caps_at_max() {
        let mut c = BitrateController::new(cfg());
        for _ in 0..50 {
            c.observe(100, 0);
        }
        assert_eq!(c.current(), 10_000);
    }

    #[test]
    fn zero_expected_is_ignored() {
        let mut c = BitrateController::new(cfg());
        assert_eq!(c.observe(0, 0), None);
    }

    #[test]
    fn hysteresis_suppresses_tiny_changes() {
        let mut c = BitrateController::new(BitrateConfig {
            // Increase of 5% < 8% hysteresis -> never emitted.
            increase: 1.05,
            clean_windows: 1,
            ..cfg()
        });
        assert_eq!(c.observe(100, 10), Some(7_500)); // big decrease still applies
        for _ in 0..10 {
            assert_eq!(c.observe(100, 0), None);
        }
        assert_eq!(c.current(), 7_500);
    }
}
