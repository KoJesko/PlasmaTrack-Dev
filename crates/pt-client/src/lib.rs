//! PlasmaTrack client library. The `plasmatrack-client` binary wires these parts
//! together; integration tests drive them directly.
//!
//! - [`listener`]: accepts the plugin's connection on 127.0.0.1, persists every
//!   frame to SQLite, then acknowledges it (docs/WIRE.md).
//! - [`db`]: the local SQLite log. The client works fully offline.
//! - [`uploader`]: sends unuploaded raw frames to the server as chunks, retrying
//!   at a fixed interval until the server accepts them.
//! - [`steam`], [`manifest`]: finds game installs, writes `plasmatrack.ini`,
//!   hashes installed files for the integrity check (rule 0).
//! - [`setup`]: installs the plugin and sets `[local]` for the installer.
//! - [`status`]: local status page.
//! - [`cli`]: the command line of both binaries.

pub mod cli;
pub mod db;
pub mod listener;
pub mod manifest;
pub mod paths;
pub mod setup;
pub mod status;
pub mod steam;
pub mod uploader;

use std::sync::{Mutex, OnceLock};

pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

static LOG_FILE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

/// Sends [`log!`] lines to `file` instead of stderr (windowless client).
pub fn set_log_file(file: std::fs::File) {
    let _ = LOG_FILE.set(Mutex::new(file));
}

#[doc(hidden)]
pub fn write_log(line: String) {
    use std::io::Write;
    match LOG_FILE.get() {
        Some(f) => {
            if let Ok(mut f) = f.lock() {
                let _ = writeln!(f, "{line}");
            }
        }
        None => eprintln!("{line}"),
    }
}

/// Timestamped log line on stderr, or in the log file once one is set.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::write_log(format!("[{}] {}", $crate::now_ms(), format!($($arg)*)))
    };
}
