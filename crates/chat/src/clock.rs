//! Where a thread's times and ids come from, so tests (and the CLI/app parity
//! scenario) get the same thread file every run.
//!
//! Normally: the system clock and random ids. With
//! `SCRIPTORIUM_TEST_CLOCK=<RFC 3339 time>` the clock stands still at that
//! time and ids are sequential (`t0001`, `t0002`, …); with
//! `SCRIPTORIUM_TEST_TIME_SCALE=<factor>` waits (retry countdowns) are
//! scaled, e.g. `0.01` for a hundred times faster. The CLI and the app both
//! call [`from_env`].

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The variable that fixes the clock and makes ids sequential.
pub const TEST_CLOCK: &str = "SCRIPTORIUM_TEST_CLOCK";
/// The variable that scales waits.
pub const TEST_TIME_SCALE: &str = "SCRIPTORIUM_TEST_TIME_SCALE";

/// The time, for a thread file's `created` and `updated`.
pub trait Clock: Send + Sync {
    /// Now, as RFC 3339 (the system clock gives UTC to the second:
    /// `2026-11-15T09:30:00Z`).
    fn now(&self) -> String;
    /// How long to really wait for a wait of `d` (a retry countdown, say).
    fn scale(&self, d: Duration) -> Duration;
}

/// Candidate thread ids. [`crate::store::ThreadStore::create`] skips any that
/// are already taken (by a thread file, or the lock file a deleted thread left
/// behind), so a source may repeat itself across runs.
pub trait IdSource: Send {
    fn next_id(&mut self) -> String;
}

/// The system clock, with waits scaled by `scale` (normally 1).
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    pub scale: f64,
}

/// A clock that always says the same time.
#[derive(Debug, Clone)]
pub struct FixedClock {
    pub at: String,
    pub scale: f64,
}

/// Random ids: `t` and 8 hex digits.
#[derive(Debug, Default)]
pub struct RandomIds {
    // Seeded randomly per instance by std.
    state: RandomState,
    n: u64,
}

/// `t0001`, `t0002`, … from 1 in each process.
#[derive(Debug, Default)]
pub struct SequentialIds {
    n: u32,
}

impl Clock for SystemClock {
    fn now(&self) -> String {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        rfc3339_utc(secs)
    }
    fn scale(&self, d: Duration) -> Duration {
        scale(d, self.scale)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> String {
        self.at.clone()
    }
    fn scale(&self, d: Duration) -> Duration {
        scale(d, self.scale)
    }
}

impl IdSource for RandomIds {
    fn next_id(&mut self) -> String {
        let mut h = self.state.build_hasher();
        h.write_u64(self.n);
        h.write_u32(std::process::id());
        h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        self.n += 1;
        format!("t{:08x}", h.finish() as u32)
    }
}

impl IdSource for SequentialIds {
    fn next_id(&mut self) -> String {
        self.n += 1;
        format!("t{:04}", self.n)
    }
}

fn scale(d: Duration, by: f64) -> Duration {
    Duration::try_from_secs_f64(d.as_secs_f64() * by).unwrap_or(Duration::MAX)
}

/// A program's clock and id source.
pub struct Clocks {
    pub clock: Box<dyn Clock>,
    pub ids: Box<dyn IdSource>,
}

/// The clock and ids the environment asks for (see the module docs). A
/// malformed variable is an error naming it, not silently ignored: a test that
/// thinks it's deterministic and isn't would fail confusingly later.
pub fn from_env() -> Result<Clocks, String> {
    let get = |k| std::env::var(k).ok();
    from_vars(get(TEST_CLOCK).as_deref(), get(TEST_TIME_SCALE).as_deref())
}

/// [`from_env`] with the variables' values passed in.
pub fn from_vars(test_clock: Option<&str>, time_scale: Option<&str>) -> Result<Clocks, String> {
    let scale = match time_scale.map(str::trim).filter(|s| !s.is_empty()) {
        None => 1.0,
        Some(s) => match s.parse::<f64>() {
            Ok(f) if (0.0..=100.0).contains(&f) => f,
            _ => return Err(format!("{TEST_TIME_SCALE}={s}: expected a number from 0 to 100")),
        },
    };
    Ok(match test_clock.map(str::trim).filter(|s| !s.is_empty()) {
        None => Clocks { clock: Box::new(SystemClock { scale }), ids: Box::new(RandomIds::default()) },
        Some(at) if is_rfc3339(at) => Clocks {
            clock: Box::new(FixedClock { at: at.to_string(), scale }),
            ids: Box::new(SequentialIds::default()),
        },
        Some(at) => return Err(format!("{TEST_CLOCK}={at}: expected an RFC 3339 time like 2026-11-15T09:30:00Z")),
    })
}

/// `secs` after the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days to civil date (proleptic Gregorian), after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `YYYY-MM-DDTHH:MM:SS`, an optional fraction, then `Z` or `±HH:MM`.
fn is_rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(digits(0..4)
        && at(4, b'-')
        && digits(5..7)
        && at(7, b'-')
        && digits(8..10)
        && (at(10, b'T') || at(10, b't'))
        && digits(11..13)
        && at(13, b':')
        && digits(14..16)
        && at(16, b':')
        && digits(17..19))
    {
        return false;
    }
    let mut i = 19;
    if at(i, b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    match b.get(i) {
        Some(b'Z' | b'z') => i + 1 == b.len(),
        Some(b'+' | b'-') => digits(i + 1..i + 3) && at(i + 3, b':') && digits(i + 4..i + 6) && i + 6 == b.len(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_794_735_045), "2026-11-15T09:30:45Z");
        assert!(is_rfc3339(&SystemClock { scale: 1.0 }.now()));
    }

    #[test]
    fn test_clock_is_fixed_and_ids_sequential() {
        let mut c = from_vars(Some("2026-11-15T09:30:00Z"), Some("0.01")).unwrap();
        assert_eq!(c.clock.now(), "2026-11-15T09:30:00Z");
        assert_eq!(c.clock.now(), "2026-11-15T09:30:00Z");
        assert_eq!(c.clock.scale(Duration::from_secs(10)), Duration::from_millis(100));
        assert_eq!([c.ids.next_id(), c.ids.next_id()], ["t0001", "t0002"]);
        // A fresh process starts again from t0001 (the store skips taken ids).
        assert_eq!(from_vars(Some("2026-11-15T09:30:00+01:00"), None).unwrap().ids.next_id(), "t0001");
    }

    #[test]
    fn real_clock_and_ids_by_default() {
        let mut c = from_vars(None, Some("")).unwrap();
        assert_eq!(c.clock.scale(Duration::from_secs(3)), Duration::from_secs(3));
        let (a, b) = (c.ids.next_id(), c.ids.next_id());
        assert_ne!(a, b);
        assert!(a.len() == 9 && a.starts_with('t') && a[1..].bytes().all(|c| c.is_ascii_hexdigit()), "{a}");
    }

    #[test]
    fn malformed_variables_are_errors() {
        for bad in ["yesterday", "2026-11-15", "2026-11-15T09:30:00", "2026-11-15T09:30:00.Z", "2026-11-15T09:30Z"] {
            let e = from_vars(Some(bad), None).err().unwrap();
            assert!(e.contains(TEST_CLOCK), "{e}");
        }
        assert!(from_vars(Some("2026-11-15T09:30:00.123Z"), None).is_ok());
        for bad in ["fast", "-1", "NaN", "1e9"] {
            let e = from_vars(None, Some(bad)).err().unwrap();
            assert!(e.contains(TEST_TIME_SCALE), "{e}");
        }
    }
}
