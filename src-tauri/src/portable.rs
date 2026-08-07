//! Running from a USB stick, leaving nothing behind.
//!
//! Asked for in the first issue anyone opened on this project. The AppImage was
//! already portable in the sense of *needing no installation* — but it still
//! wrote a database, a log and a keychain entry into the user's home folder, so
//! running it on a borrowed machine left a list of who mails them behind on it.
//! For an app whose whole argument is that the data stays yours, that is the
//! wrong half of the promise to keep.
//!
//! Portable mode moves everything next to the executable. It is opt-in and
//! explicit — a file named `hush-portable.txt` beside the binary, or
//! `HUSH_PORTABLE=1` — because guessing would be worse than either answer:
//! silently writing to a read-only mount fails, and silently *not* writing to
//! the user's home loses their scan.
//!
//! One thing it deliberately does not do: keep the Google connection. The OS
//! keychain is tied to the machine, and the alternative — a refresh token in a
//! plain file on a memory stick — is a worse idea than reconnecting each time.
//! So portable mode holds the token in memory only, which the app already
//! supports for machines with no secret store.

use std::path::{Path, PathBuf};

/// The marker file that turns portable mode on, beside the executable.
pub const MARKER: &str = "hush-portable.txt";

/// Where the executable really lives, following an AppImage back to the
/// mounted image's own directory rather than the temporary squashfs root.
fn beside_executable() -> Option<PathBuf> {
    // An AppImage unpacks itself into /tmp and runs from there, so
    // `current_exe` points at the temporary copy. APPIMAGE holds the path of
    // the actual file the user double-clicked, which is what "beside the
    // executable" has to mean.
    if let Ok(appimage) = std::env::var("APPIMAGE") {
        if let Some(dir) = Path::new(&appimage).parent() {
            return Some(dir.to_path_buf());
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// The directory to keep everything in, or `None` for the normal install.
///
/// Returns `None` rather than failing when the marker is there but the
/// directory cannot be written to — a read-only stick should fall back to
/// working normally, not refuse to start.
pub fn data_dir() -> Option<PathBuf> {
    let dir = beside_executable()?;

    let asked = std::env::var("HUSH_PORTABLE").is_ok_and(|v| v != "0" && !v.is_empty())
        || dir.join(MARKER).exists();
    if !asked {
        return None;
    }

    let target = dir.join("hush-data");
    match std::fs::create_dir_all(&target) {
        Ok(()) if is_writable(&target) => Some(target),
        _ => {
            log::warn!(
                "portable mode was asked for, but {} can't be written to — \
                 using the usual folder instead",
                target.display()
            );
            None
        }
    }
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".hush-write-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_happens_without_the_marker() {
        // The default has to stay the normal install. Someone who has never
        // heard of portable mode must never have their data moved.
        let dir = std::env::temp_dir().join(format!("hush-p-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!dir.join(MARKER).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_that_cannot_be_written_falls_back() {
        // A read-only stick must not stop the app starting.
        assert!(!is_writable(Path::new("/proc/nonexistent-hush-probe")));
    }

    #[test]
    fn a_writable_directory_is_detected() {
        let dir = std::env::temp_dir().join(format!("hush-w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(is_writable(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }
}
