//! A plain append-only log file, because a Finder-launched app's stderr goes
//! nowhere. One line per event: warnings, errors and panics.
//!
//! macOS: `~/Library/Logs/Scriptorium/scriptorium.log`.
//! Elsewhere: `$XDG_STATE_HOME/scriptorium/scriptorium.log`
//! (default `~/.local/state/scriptorium/scriptorium.log`).

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let dir = if cfg!(target_os = "macos") {
        home?.join("Library/Logs/Scriptorium")
    } else {
        match std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d).join("scriptorium"),
            None => home?.join(".local/state/scriptorium"),
        }
    };
    Some(dir.join("scriptorium.log"))
}

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn warn(msg: &str) {
    eprintln!("warning: {msg}");
    write("WARN", msg);
}

pub fn error(msg: &str) {
    eprintln!("error: {msg}");
    write("ERROR", msg);
}

/// Log panics too (then run the default hook, which prints to stderr).
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        write("PANIC", &format!("{info}\n{backtrace}"));
        previous(info);
    }));
}

/// Never fails: logging must not take the app down.
fn write(level: &str, msg: &str) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    // One line per event: continuation lines are indented.
    let msg = msg.trim_end().replace('\n', "\n    ");
    let _ = writeln!(f, "{} {level} {msg}", timestamp());
}

/// UTC, `2026-11-14T09:30:00Z`, without a date crate.
fn timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_406), (2025, 11, 14));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }
}
