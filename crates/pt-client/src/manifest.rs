//! Install manifest for the integrity check (rule 0, PLAN §6a): SHA-256 of the
//! client binary, the config file and every installed PlasmaTrack plugin.

use std::io::Read;
use std::path::Path;

use pt_core::upload::ManifestEntry;
use sha2::{Digest, Sha256};

use crate::steam::GameInstall;

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn compute(client_binary: Option<&Path>, config_file: Option<&Path>, installs: &[GameInstall]) -> Vec<ManifestEntry> {
    let mut out = Vec::new();
    let mut add = |role: &str, path: &Path| {
        if let Ok(sha256) = sha256_file(path) {
            out.push(ManifestEntry { role: role.to_string(), path: path.display().to_string(), sha256 });
        }
    };
    if let Some(p) = client_binary {
        add("client_binary", p);
    }
    if let Some(p) = config_file {
        add("config_file", p);
    }
    for i in installs {
        let p = i.plugin_path();
        if p.is_file() {
            add("plugin", &p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_files_and_skips_missing_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("plasmatrack.toml");
        std::fs::write(&cfg, "abc").unwrap();
        assert_eq!(
            sha256_file(&cfg).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let install = GameInstall { game: pt_core::Game::Ats, plugins_dir: plugins.clone(), windows_build: true };
        let m = compute(None, Some(&cfg), std::slice::from_ref(&install));
        assert_eq!(m.len(), 1);
        std::fs::write(plugins.join("plasmatrack.dll"), "dll").unwrap();
        let m = compute(None, Some(&cfg), &[install]);
        assert_eq!(m.iter().map(|e| e.role.as_str()).collect::<Vec<_>>(), vec!["config_file", "plugin"]);
    }
}
