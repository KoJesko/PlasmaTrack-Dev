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
//! - [`status`]: local status page.

pub mod db;
pub mod listener;
pub mod manifest;
pub mod paths;
pub mod status;
pub mod steam;
pub mod uploader;

pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Timestamped log line on stderr.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("[{}] {}", $crate::now_ms(), format!($($arg)*))
    };
}
