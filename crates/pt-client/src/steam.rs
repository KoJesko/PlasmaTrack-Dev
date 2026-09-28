//! Finds ETS2/ATS installs in every Steam library (native Steam, Flatpak and
//! Snap Steam on Linux) and maintains `plasmatrack.ini` next to the plugin.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pt_core::Game;

pub const PLUGIN_FILE_WINDOWS: &str = "plasmatrack.dll";
pub const PLUGIN_FILE_LINUX: &str = "plasmatrack.so";
pub const INI_FILE: &str = "plasmatrack.ini";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GameInstall {
    pub game: Game,
    /// `bin/win_x64/plugins` (Windows DLL, also used under Proton) or
    /// `bin/linux_x64/plugins` (native Linux build).
    pub plugins_dir: PathBuf,
    pub windows_build: bool,
}

impl GameInstall {
    pub fn plugin_path(&self) -> PathBuf {
        self.plugins_dir.join(if self.windows_build { PLUGIN_FILE_WINDOWS } else { PLUGIN_FILE_LINUX })
    }
}

fn game_folder(game: Game) -> &'static str {
    match game {
        Game::Ets2 => "Euro Truck Simulator 2",
        Game::Ats => "American Truck Simulator",
    }
}

/// Candidate Steam installation roots for this OS.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        if let Ok(key) = winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam") {
            if let Ok(p) = key.get_value::<String, _>("SteamPath") {
                roots.push(PathBuf::from(p));
            }
        }
        roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = dirs::home_dir() {
            for rel in [
                ".local/share/Steam",
                ".steam/steam",
                ".var/app/com.valvesoftware.Steam/.local/share/Steam", // Flatpak Steam
                "snap/steam/common/.local/share/Steam",                 // Snap Steam
            ] {
                roots.push(home.join(rel));
            }
        }
    }
    roots
}

/// Extracts the `"path"` values from Steam's `libraryfolders.vdf`.
pub fn parse_library_paths(vdf: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in vdf.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("\"path\"") else { continue };
        let rest = rest.trim();
        let Some(inner) = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else { continue };
        out.push(PathBuf::from(inner.replace("\\\\", "\\")));
    }
    out
}

/// Every Steam library folder reachable from the known roots.
pub fn library_folders(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut libs = BTreeSet::new();
    for root in roots {
        if !root.join("steamapps").is_dir() {
            continue;
        }
        libs.insert(normalize(root));
        if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
            for p in parse_library_paths(&text) {
                if p.join("steamapps").is_dir() {
                    libs.insert(normalize(&p));
                }
            }
        }
    }
    libs.into_iter().collect()
}

fn normalize(p: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // Windows canonical paths are verbatim (`\\?\C:\...`); keep plain drive paths
    // readable in logs and manifests. UNC paths stay verbatim.
    let s = canon.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => canon,
    }
}

/// All ETS2/ATS installs in the given libraries.
pub fn find_installs(libraries: &[PathBuf]) -> Vec<GameInstall> {
    let mut out = Vec::new();
    for lib in libraries {
        for game in [Game::Ets2, Game::Ats] {
            let bin = lib.join("steamapps").join("common").join(game_folder(game)).join("bin");
            for (sub, windows_build) in [("win_x64", true), ("linux_x64", false)] {
                if bin.join(sub).is_dir() {
                    out.push(GameInstall { game, plugins_dir: bin.join(sub).join("plugins"), windows_build });
                }
            }
        }
    }
    out.sort();
    out
}

pub fn discover() -> Vec<GameInstall> {
    find_installs(&library_folders(&steam_roots()))
}

/// Keys the client manages in `plasmatrack.ini`; every other line (e.g.
/// `capture = 1`, comments) is preserved.
pub fn merge_ini(existing: &str, managed: &[(&str, String)]) -> String {
    let mut out = String::new();
    let mut written = BTreeSet::new();
    for line in existing.lines() {
        let key = line.split(['=', '#', ';']).next().unwrap_or("").trim();
        if let Some((k, v)) = managed.iter().find(|(k, _)| *k == key && line.contains('=')) {
            if written.insert(*k) {
                out.push_str(&format!("{k} = {v}\n"));
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    for (k, v) in managed {
        if !written.contains(k) {
            out.push_str(&format!("{k} = {v}\n"));
        }
    }
    out
}

/// Writes/updates `plasmatrack.ini` in a plugins folder. Returns true if the
/// file changed. The folder must already exist (the plugin is installed there).
pub fn write_ini(plugins_dir: &Path, managed: &[(&str, String)]) -> std::io::Result<bool> {
    let path = plugins_dir.join(INI_FILE);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge_ini(&existing, managed);
    if merged == existing {
        return Ok(false);
    }
    std::fs::write(&path, merged)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_libraryfolders_vdf() {
        let vdf = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"apps"
		{
			"227300"		"30106738302"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        assert_eq!(
            parse_library_paths(vdf),
            vec![PathBuf::from(r"C:\Program Files (x86)\Steam"), PathBuf::from(r"D:\SteamLibrary")]
        );
    }

    #[test]
    fn ini_merge_preserves_unmanaged_lines() {
        let existing = "; comment\ncapture = 1\nport = 1234\njournal_max_mib = 8\n";
        let merged = merge_ini(existing, &[("port", "30455".into()), ("sample_rate_hz", "20".into())]);
        assert_eq!(merged, "; comment\ncapture = 1\nport = 30455\njournal_max_mib = 8\nsample_rate_hz = 20\n");
        // Idempotent.
        assert_eq!(merge_ini(&merged, &[("port", "30455".into()), ("sample_rate_hz", "20".into())]), merged);
    }

    #[test]
    fn finds_installs_in_a_fake_library() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().to_path_buf();
        std::fs::create_dir_all(lib.join("steamapps/common/American Truck Simulator/bin/win_x64")).unwrap();
        std::fs::create_dir_all(lib.join("steamapps/common/Euro Truck Simulator 2/bin/linux_x64")).unwrap();
        let installs = find_installs(std::slice::from_ref(&lib));
        assert_eq!(installs.len(), 2);
        assert!(installs.iter().any(|i| i.game == Game::Ats && i.windows_build));
        assert!(installs.iter().any(|i| i.game == Game::Ets2 && !i.windows_build));
        let ats = installs.iter().find(|i| i.game == Game::Ats).unwrap();
        assert!(ats.plugin_path().ends_with("plugins/plasmatrack.dll") || ats.plugin_path().ends_with("plugins\\plasmatrack.dll"));
    }
}
