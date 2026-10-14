//! Install steps behind the `install-plugin`, `uninstall-plugin` and `set-local`
//! commands. The installer (installer/, Electron) only calls these commands, so
//! everything that touches game folders or the config is here and tested.

use std::path::{Path, PathBuf};

use pt_core::{Config, Game};
use serde::Serialize;

use crate::steam::{self, GameInstall};

/// What happened in one plugins folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderResult {
    pub game: Game,
    pub plugins_dir: PathBuf,
    /// The folder's contents changed.
    pub changed: bool,
    /// Why this folder failed; the other folders are still handled.
    pub error: Option<String>,
}

fn same_file_contents(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn io_hint(e: &std::io::Error) -> String {
    match e.kind() {
        // A loaded DLL cannot be overwritten while the game runs.
        std::io::ErrorKind::PermissionDenied => format!("{e} (close the game; if it persists, run as administrator)"),
        _ => e.to_string(),
    }
}

/// Copies `plugin` (plasmatrack.dll or plasmatrack.so) into every install built
/// for it, then writes `plasmatrack.ini` there. Returns one result per folder.
pub fn install_plugin(installs: &[GameInstall], plugin: &Path, ini: &[(&str, String)]) -> Vec<FolderResult> {
    let is_dll = plugin.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll"));
    installs
        .iter()
        .filter(|i| i.windows_build == is_dll)
        .map(|i| {
            let dest = i.plugin_path();
            let copied = if same_file_contents(plugin, &dest) {
                Ok(false)
            } else {
                std::fs::create_dir_all(&i.plugins_dir).and_then(|_| std::fs::copy(plugin, &dest)).map(|_| true)
            };
            let result = copied.and_then(|c| steam::write_ini(&i.plugins_dir, ini).map(|w| c || w));
            FolderResult {
                game: i.game,
                plugins_dir: i.plugins_dir.clone(),
                changed: *result.as_ref().unwrap_or(&false),
                error: result.err().map(|e| io_hint(&e)),
            }
        })
        .collect()
}

/// Removes the plugin and `plasmatrack.ini` from every install. The plugin's
/// `plasmatrack/` folder (journal, captures) is left for the player to delete.
pub fn uninstall_plugin(installs: &[GameInstall]) -> Vec<FolderResult> {
    installs
        .iter()
        .map(|i| {
            let mut changed = false;
            let mut error = None;
            for p in [i.plugin_path(), i.plugins_dir.join(steam::INI_FILE)] {
                match std::fs::remove_file(&p) {
                    Ok(()) => changed = true,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => error = Some(io_hint(&e)),
                }
            }
            FolderResult { game: i.game, plugins_dir: i.plugins_dir.clone(), changed, error }
        })
        .collect()
}

/// Replaces a value in place, so the comment above its key stays.
fn set(table: &mut dyn toml_edit::TableLike, key: &str, v: &str) {
    match table.get_mut(key) {
        Some(item) => *item = toml_edit::value(v),
        None => {
            table.insert(key, toml_edit::value(v));
        }
    }
}

/// Sets `[local] server_url` and/or `token` in a config file's text. Comments
/// and every other key are kept as they are, so the config still matches the
/// published version (rule 0 ignores `[local]`).
pub fn set_local(config_text: &str, server_url: Option<&str>, token: Option<&str>) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = config_text.parse().map_err(|e| format!("config: {e}"))?;
    let local = doc
        .get_mut("local")
        .and_then(|t| t.as_table_like_mut())
        .ok_or("config has no [local] table")?;
    if let Some(url) = server_url {
        let url = url.trim().trim_end_matches('/');
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("server URL must start with https://".into());
        }
        set(local, "server_url", url);
    }
    if let Some(token) = token {
        let token = token.trim();
        if token.chars().any(|c| c.is_control()) {
            return Err("token contains control characters".into());
        }
        set(local, "token", token);
    }
    let text = doc.to_string();
    Config::from_toml_str(&text).map_err(|e| format!("config: {e}"))?;
    Ok(text)
}

/// The shipped config with the `[local]` values of an existing config carried
/// over. Everything else must match a published config version (rule 0), so an
/// install or update always starts from the config it ships; an old file that
/// lacks newer keys, or no longer parses, cannot stop the client. Values of
/// `[local]` keys the shipped config does not have are dropped.
pub fn rebase_config(existing: Option<&str>, shipped: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = shipped.parse().map_err(|e| format!("shipped config: {e}"))?;
    let old: Option<toml_edit::DocumentMut> = existing.and_then(|t| t.parse().ok());
    if let (Some(old_local), Some(new_local)) = (
        old.as_ref().and_then(|d| d.get("local")).and_then(|t| t.as_table_like()),
        doc.get_mut("local").and_then(|t| t.as_table_like_mut()),
    ) {
        let keys: Vec<String> = new_local.iter().map(|(k, _)| k.to_string()).collect();
        for k in keys {
            if let Some(v) = old_local.get(&k).and_then(|i| i.as_value()) {
                // Keep the shipped line's decoration (comments stay where they are).
                let mut v = v.clone();
                v.decor_mut().clear();
                if let Some(item) = new_local.get_mut(&k) {
                    *item = toml_edit::Item::Value(v);
                }
            }
        }
    }
    let text = doc.to_string();
    Config::from_toml_str(&text).map_err(|e| format!("config: {e}"))?;
    Ok(text)
}

/// Writes `text` to `path` through a temporary file, so a crash never leaves a
/// half-written config.
pub fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::SAMPLE_CONFIG;

    fn fake_installs(root: &Path) -> Vec<GameInstall> {
        vec![
            GameInstall { game: Game::Ats, plugins_dir: root.join("ats/bin/win_x64/plugins"), windows_build: true },
            GameInstall { game: Game::Ets2, plugins_dir: root.join("ets2/bin/win_x64/plugins"), windows_build: true },
            GameInstall { game: Game::Ets2, plugins_dir: root.join("ets2/bin/linux_x64/plugins"), windows_build: false },
        ]
    }

    #[test]
    fn installs_into_matching_builds_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("plasmatrack.dll");
        std::fs::write(&dll, "dll v1").unwrap();
        let installs = fake_installs(dir.path());
        let ini = [("port", "30455".to_string())];

        let r = install_plugin(&installs, &dll, &ini);
        assert_eq!(r.len(), 2, "the .so folder is skipped");
        assert!(r.iter().all(|f| f.changed && f.error.is_none()));
        assert_eq!(std::fs::read_to_string(installs[0].plugin_path()).unwrap(), "dll v1");
        assert_eq!(std::fs::read_to_string(installs[1].plugins_dir.join("plasmatrack.ini")).unwrap(), "port = 30455\n");
        assert!(!installs[2].plugins_dir.exists());

        assert!(install_plugin(&installs, &dll, &ini).iter().all(|f| !f.changed));
        std::fs::write(&dll, "dll v2").unwrap();
        assert!(install_plugin(&installs, &dll, &ini).iter().all(|f| f.changed));
        assert_eq!(std::fs::read_to_string(installs[1].plugin_path()).unwrap(), "dll v2");
    }

    #[test]
    fn install_keeps_unmanaged_ini_lines() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("plasmatrack.dll");
        std::fs::write(&dll, "dll").unwrap();
        let installs = fake_installs(dir.path());
        std::fs::create_dir_all(&installs[0].plugins_dir).unwrap();
        std::fs::write(installs[0].plugins_dir.join("plasmatrack.ini"), "capture = 1\nport = 1\n").unwrap();
        install_plugin(&installs[..1], &dll, &[("port", "30455".into())]);
        assert_eq!(
            std::fs::read_to_string(installs[0].plugins_dir.join("plasmatrack.ini")).unwrap(),
            "capture = 1\nport = 30455\n"
        );
    }

    #[test]
    fn uninstall_removes_plugin_and_ini_only() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("plasmatrack.dll");
        std::fs::write(&dll, "dll").unwrap();
        let installs = fake_installs(dir.path());
        install_plugin(&installs, &dll, &[("port", "30455".into())]);
        let journal = installs[0].plugins_dir.join("plasmatrack");
        std::fs::create_dir_all(&journal).unwrap();

        let r = uninstall_plugin(&installs);
        assert_eq!(r.iter().map(|f| f.changed).collect::<Vec<_>>(), [true, true, false]);
        assert!(r.iter().all(|f| f.error.is_none()));
        assert!(!installs[0].plugin_path().exists());
        assert!(!installs[0].plugins_dir.join("plasmatrack.ini").exists());
        assert!(journal.is_dir());
    }

    #[test]
    fn set_local_keeps_comments_and_integrity_hash() {
        let text = set_local(SAMPLE_CONFIG, Some("https://pt.example.org/ "), Some(" abc123 ")).unwrap();
        let before = Config::from_toml_str(SAMPLE_CONFIG).unwrap();
        let after = Config::from_toml_str(&text).unwrap();
        assert_eq!(after.local.server_url, "https://pt.example.org");
        assert_eq!(after.local.token, "abc123");
        assert_eq!(after.integrity_hash(), before.integrity_hash());
        assert!(text.contains("# Player token issued by the server"));
        assert_eq!(text.lines().count(), SAMPLE_CONFIG.lines().count());

        // Token only: the URL stays.
        let again = set_local(&text, None, Some("xyz")).unwrap();
        assert_eq!(Config::from_toml_str(&again).unwrap().local.server_url, "https://pt.example.org");
    }

    #[test]
    fn rebase_keeps_local_and_takes_everything_else_from_the_shipped_config() {
        let mine = set_local(SAMPLE_CONFIG, Some("https://pt.example.org"), Some("tok")).unwrap();
        // An old config: a rule changed, a newer key missing, a custom port.
        let old = mine
            .replace("speed_penalty_factor = 0.85", "speed_penalty_factor = 0.5")
            .replace("port = 30455", "port = 31111")
            .lines()
            .filter(|l| !l.starts_with("delivered_distance_resolution_km"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(Config::from_toml_str(&old).is_err(), "the old file no longer loads");
        let text = rebase_config(Some(&old), SAMPLE_CONFIG).unwrap();
        let cfg = Config::from_toml_str(&text).unwrap();
        assert_eq!(cfg.integrity_hash(), Config::from_toml_str(SAMPLE_CONFIG).unwrap().integrity_hash());
        assert_eq!((cfg.local.server_url.as_str(), cfg.local.token.as_str(), cfg.local.port), ("https://pt.example.org", "tok", 31111));
        assert!(text.contains("# Player token issued by the server"));
        // Nothing to carry over: the shipped config as is.
        assert_eq!(rebase_config(None, SAMPLE_CONFIG).unwrap(), SAMPLE_CONFIG);
        assert_eq!(rebase_config(Some("not toml ["), SAMPLE_CONFIG).unwrap(), SAMPLE_CONFIG);
    }

    #[test]
    fn set_local_rejects_bad_input() {
        assert!(set_local(SAMPLE_CONFIG, Some("pt.example.org"), None).is_err());
        assert!(set_local(SAMPLE_CONFIG, None, Some("a\nb")).is_err());
        assert!(set_local("[sync]\n", None, Some("a")).is_err());
    }
}
