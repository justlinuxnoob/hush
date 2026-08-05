//! A log file, so failures can be explained rather than guessed at.
//!
//! Every `log::warn!` in this codebase used to go nowhere: no logger was ever
//! installed, so when Google refused a request the reason was discarded and the
//! user was left with an app that silently did nothing. Diagnosing that from
//! the outside is impossible, which is exactly what happened.
//!
//! What is written: what Hush tried to do and what came back. Sender addresses
//! appear, because a failure that does not say which sender is useless. Message
//! contents never do, for the same reason they are never fetched.
//!
//! The file lives beside the database, is capped, and is deleted along with
//! everything else by "Erase everything".

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{Level, LevelFilter, Metadata, Record};

/// Past this size the file is rotated to `.old`, keeping at most two.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub const LOG_FILE: &str = "hush.log";

struct FileLogger {
    path: PathBuf,
    handle: Mutex<Option<std::fs::File>>,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {} — {}\n",
            timestamp(),
            record.level(),
            record.target(),
            record.args()
        );

        // A logger that panics is worse than a missing log line, so every
        // failure here is swallowed deliberately.
        let Ok(mut guard) = self.handle.lock() else {
            return;
        };
        if guard.is_none() {
            *guard = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.handle.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// Start logging to `dir/hush.log`, returning where it went.
///
/// Safe to call twice; the second call is a no-op rather than a panic.
pub fn init(dir: &Path) -> PathBuf {
    let path = dir.join(LOG_FILE);
    rotate_if_large(&path);

    let logger = Box::new(FileLogger {
        path: path.clone(),
        handle: Mutex::new(None),
    });

    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Info);
        log::info!(
            "hush {} starting on {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        );
    }
    path
}

fn rotate_if_large(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() > MAX_BYTES {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
}

fn timestamp() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_log_is_rotated_rather_than_growing_forever() {
        let dir = std::env::temp_dir().join(format!("hush-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hush.log");

        std::fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        rotate_if_large(&path);

        assert!(!path.exists(), "the oversized file was moved aside");
        assert!(path.with_extension("log.old").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_small_log_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("hush-log-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hush.log");

        std::fs::write(&path, b"a line").unwrap();
        rotate_if_large(&path);

        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timestamps_are_sortable() {
        let t = timestamp();
        assert!(t.contains('T'), "{t}");
        assert!(t.len() > 15, "{t}");
    }
}
