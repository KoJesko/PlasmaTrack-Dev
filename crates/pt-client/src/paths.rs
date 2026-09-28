//! Default locations. Windows: `%APPDATA%\PlasmaTrack\plasmatrack.toml` and
//! `%LOCALAPPDATA%\PlasmaTrack\client.sqlite`. Linux: `~/.config/plasmatrack/`
//! and `~/.local/share/plasmatrack/`.

use std::path::PathBuf;

fn app_dir(base: Option<PathBuf>) -> PathBuf {
    let name = if cfg!(windows) { "PlasmaTrack" } else { "plasmatrack" };
    base.unwrap_or_else(|| PathBuf::from(".")).join(name)
}

pub fn default_config_path() -> PathBuf {
    app_dir(dirs::config_dir()).join("plasmatrack.toml")
}

pub fn default_data_dir() -> PathBuf {
    app_dir(dirs::data_local_dir())
}

pub const DB_FILE: &str = "client.sqlite";

/// The sample config shipped with the client; `init-config` writes it.
pub const SAMPLE_CONFIG: &str = include_str!("../../../config/plasmatrack.sample.toml");
