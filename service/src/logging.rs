use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn log_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    PathBuf::from(base).join("Warden")
}

pub fn log_path() -> PathBuf {
    log_dir().join("service.log")
}

/// Best-effort file logging. Failures here are swallowed on purpose — a
/// logging failure should never be the thing that brings the service down.
pub fn log_line(msg: &str) {
    let dir = log_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(file, "[{timestamp}] {msg}");
    }
}
