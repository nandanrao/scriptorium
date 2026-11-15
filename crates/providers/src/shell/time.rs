//! Waiting, scaled for tests.
//!
//! Retry waits ([`Action::Wait`](scriptorium_agent::turn::Action::Wait)) and
//! replayed delays go through a [`TimeScale`], so a test can run a "retry in
//! 4 s" in 40 ms (`SCRIPTORIUM_TEST_TIME_SCALE=0.01`) or not at all (`0`).

use std::time::{Duration, Instant};

use super::CancelToken;

/// The environment variable that scales waits (a non-negative number; 1 is
/// real time).
pub const TIME_SCALE_ENV: &str = "SCRIPTORIUM_TEST_TIME_SCALE";

/// The longest a wait sleeps between checks of its [`CancelToken`].
const TICK: Duration = Duration::from_millis(50);

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

    /// Sleeps for `d`, scaled, checking `cancel` every 50 ms or less.
    /// Returns false if it was cancelled.
    pub fn sleep(self, d: Duration, cancel: &CancelToken) -> bool {
        let end = Instant::now() + self.scale(d);
        loop {
            if cancel.is_cancelled() {
                return false;
            }
            let now = Instant::now();
            if now >= end {
                return true;
            }
            std::thread::sleep((end - now).min(TICK));
        }
    }

    /// The wait for [`Action::Wait`](scriptorium_agent::turn::Action::Wait):
    /// `secs` plus up to a quarter more at random (so clients that failed
    /// together don't retry together), scaled. Returns false if cancelled
    /// (the writer pressed Stop during the countdown).
    pub fn sleep_unless(self, secs: u64, cancel: &CancelToken) -> bool {
        let base = Duration::from_secs(secs);
        self.sleep(base + base.mul_f64(jitter() * 0.25), cancel)
    }
}

/// A number in [0, 1), different each call (std's per-process random hash
/// keys plus the clock: plenty for spreading retries; not for secrets).
fn jitter() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos());
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
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
        assert!(TimeScale::INSTANT.sleep_unless(300, &CancelToken::new()));
        assert!(TimeScale::INSTANT.sleep(Duration::from_secs(300), &CancelToken::new()));
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn retry_waits_have_jitter_within_a_quarter() {
        let scale = TimeScale::new(0.01);
        let t = Instant::now();
        assert!(scale.sleep_unless(4, &CancelToken::new()));
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(40) && took < Duration::from_millis(500), "{took:?}");
        let js: Vec<f64> = (0..20).map(|_| jitter()).collect();
        assert!(js.iter().all(|j| (0.0..1.0).contains(j)));
        assert!(js.iter().any(|j| *j != js[0]));
    }

    #[test]
    fn cancel_ends_a_wait() {
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let t = Instant::now();
        let h = std::thread::spawn(move || TimeScale::REAL.sleep_unless(300, &c));
        std::thread::sleep(Duration::from_millis(20));
        cancel.cancel();
        assert!(!h.join().unwrap());
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}
