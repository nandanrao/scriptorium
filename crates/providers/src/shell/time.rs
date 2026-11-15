//! Waiting, scaled for tests.
//!
//! Retry waits ([`Action::Wait`](scriptorium_agent::turn::Action::Wait)) and
//! replayed delays go through a [`TimeScale`], so a test can run a "retry in
//! 4 s" in 40 ms (`SCRIPTORIUM_TEST_TIME_SCALE=0.01`) or not at all (`0`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The environment variable that scales waits (a non-negative number; 1 is
/// real time).
pub const TIME_SCALE_ENV: &str = "SCRIPTORIUM_TEST_TIME_SCALE";

/// A factor applied to every wait.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeScale(f64);

impl Default for TimeScale {
    fn default() -> Self {
        TimeScale::REAL
    }
}

impl TimeScale {
    pub const REAL: TimeScale = TimeScale(1.0);
    /// No waiting at all.
    pub const INSTANT: TimeScale = TimeScale(0.0);

    /// `factor` if it's a finite non-negative number, else real time.
    pub fn new(factor: f64) -> Self {
        if factor.is_finite() && factor >= 0.0 { TimeScale(factor) } else { TimeScale::REAL }
    }

    /// From `SCRIPTORIUM_TEST_TIME_SCALE`; real time when unset or unreadable.
    pub fn from_env() -> Self {
        std::env::var(TIME_SCALE_ENV).ok().and_then(|v| v.trim().parse().ok()).map_or(TimeScale::REAL, TimeScale::new)
    }

    pub fn factor(self) -> f64 {
        self.0
    }

    /// `d`, scaled.
    pub fn scale(self, d: Duration) -> Duration {
        if self.0 == 1.0 { d } else { d.mul_f64(self.0) }
    }

    /// Sleeps for `d`, scaled.
    pub fn sleep(self, d: Duration) {
        let d = self.scale(d);
        if !d.is_zero() {
            std::thread::sleep(d);
        }
    }

    /// Sleeps for `secs` seconds, scaled, waking every 50 ms or less to check
    /// `stop`. Returns false if `stop` was set (the writer pressed Stop
    /// during a retry countdown).
    pub fn sleep_unless(self, secs: u64, stop: &AtomicBool) -> bool {
        let end = Instant::now() + self.scale(Duration::from_secs(secs));
        loop {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            let now = Instant::now();
            if now >= end {
                return true;
            }
            std::thread::sleep((end - now).min(Duration::from_millis(50)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales() {
        assert_eq!(TimeScale::new(0.01).scale(Duration::from_secs(4)), Duration::from_millis(40));
        assert_eq!(TimeScale::INSTANT.scale(Duration::from_secs(300)), Duration::ZERO);
        assert_eq!(TimeScale::new(-1.0), TimeScale::REAL);
        assert_eq!(TimeScale::new(f64::NAN), TimeScale::REAL);
    }

    #[test]
    fn a_long_retry_wait_takes_no_time_when_scaled_to_zero() {
        let t = Instant::now();
        assert!(TimeScale::INSTANT.sleep_unless(300, &AtomicBool::new(false)));
        TimeScale::INSTANT.sleep(Duration::from_secs(300));
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn stop_ends_a_wait() {
        let t = Instant::now();
        assert!(!TimeScale::REAL.sleep_unless(300, &AtomicBool::new(true)));
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
