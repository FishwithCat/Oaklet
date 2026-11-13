//! Key diagnostics that outlive the process.
//!
//! A release build has no console and `panic = "abort"`, so the lines a user
//! would need to explain a failure are appended to the platform log dir.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static PATH: OnceLock<PathBuf> = OnceLock::new();

/// Send key logs to `path`, creating its directory. Call once at startup.
pub fn init(path: PathBuf) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = PATH.set(path);
}

/// Record one key line on stderr and, once initialized, in the log file.
pub fn key(message: fmt::Arguments<'_>) {
    eprintln!("{message}");
    if let Some(path) = PATH.get() {
        append(path, message);
    }
}

// ponytail: append-only, never rotated; key logs are rare. Cap or rotate if
// that stops being true.
fn append(path: &Path, message: fmt::Arguments<'_>) {
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{} {message}", timestamp());
}

fn timestamp() -> String {
    format_utc(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
    )
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ`, so the file needs no date crate.
fn format_utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn appends_one_line_per_key() {
        let path = std::env::temp_dir().join(format!("oaklet-log-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        append(&path, format_args!("first"));
        append(&path, format_args!("second 42"));
        let text = std::fs::read_to_string(&path).expect("log file");
        let _ = std::fs::remove_file(&path);

        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(" first"));
        assert!(lines[1].ends_with(" second 42"));
        assert!(lines[0].split(' ').next().unwrap().ends_with('Z'));
    }
}
